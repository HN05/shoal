//! Tell the hub before the machine sleeps. The daemon learns of a coming
//! sleep from the platform, which waits for it a while: logind's
//! `PrepareForSleep` under a sleep delay lock on Linux, IOKit's will-sleep
//! notification on macOS, whose acknowledgement it may delay. From then
//! until the machine fully wakes, the agent queue admits nothing, and the
//! hub link sends the notice and packages within the platform's allowance,
//! then stays closed, so a dark wake, as macOS's Power Nap makes, neither
//! starts work nor tells the hub the machine is back. Meanwhile the runner
//! ends its running CI jobs to rerun, so other runners can take them, and
//! running reviews stop to run again once the machine wakes.
use std::{sync::Arc, time::Duration};

use tokio::time::Instant;

use super::{log, manager::Manager};
use shoal_daemon_base::jobs::JobStop;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "linux")]
mod logind;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "macos")]
use iokit::connect;
#[cfg(target_os = "linux")]
use logind::connect;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use unsupported::connect;

/// What the platform announces of the machine's sleep.
pub(super) enum PowerEvent<H> {
    /// The machine goes to sleep once `hold` drops, or once `allowance`
    /// passes, without a hold where the platform gave none.
    Sleeping {
        hold: Option<H>,
        allowance: Duration,
    },
    /// The machine fully woke.
    Woke,
}

/// The platform's sleep announcements.
pub(super) trait PowerEvents {
    /// Holds a coming sleep off until dropped.
    type Hold;

    /// The next announcement; none once announcements end.
    async fn next(&mut self) -> Option<PowerEvent<Self::Hold>>;
}

/// The part of a sleep's allowance the daemon leaves the platform, so the
/// hold drops before the platform gives up waiting for it.
const MARGIN: Duration = Duration::from_secs(1);

/// The share of what is left of a sleep's allowance a hand-over before it
/// may take, so the hub's sleep notice still goes out after it.
const HANDOVER_SHARE: f64 = 0.75;

pub(super) async fn run(manager: Arc<Manager>) {
    if let Some(events) = connect().await {
        follow(&manager, events).await;
        log!("sleep lock: the platform stopped announcing sleep");
    }
}

/// Act on each announcement of a sleep or wake until announcements end.
async fn follow(manager: &Arc<Manager>, mut events: impl PowerEvents) {
    while let Some(event) = events.next().await {
        match event {
            PowerEvent::Sleeping { hold, allowance } => {
                let deadline = Instant::now() + allowance.saturating_sub(MARGIN.min(allowance / 5));
                prepare_for_sleep(manager, deadline).await;
                drop(hold);
            }
            PowerEvent::Woke => woke(manager).await,
        }
    }
}

/// Hold the agent queue, queue running reviews again, under
/// `keep_awake = "handover"` hand the spaces with something to do over
/// within [`HANDOVER_SHARE`] of the time left, and by `deadline` end the
/// running CI jobs to rerun, handing the hub those cut off, and tell the
/// hub that the machine sleeps, then close its link until it wakes.
async fn prepare_for_sleep(manager: &Arc<Manager>, deadline: Instant) {
    log!("sleep lock: the machine goes to sleep");
    manager.agents.agent_queue().sleep(true);
    manager.reviews.stop_for_sleep().await;
    let jobs = end_jobs(manager, deadline);
    let announced = async {
        (manager.agents)
            .hand_over_before_sleep(&**manager, handover_deadline(deadline))
            .await;
        manager.hub.announce_sleep(&**manager, deadline).await;
    };
    tokio::join!(announced, jobs);
    manager.hub.slept();
}

/// End the running CI jobs to rerun, waiting for that until `deadline`; a
/// stop the sleep interrupts goes on once the machine wakes.
async fn end_jobs(manager: &Arc<Manager>, deadline: Instant) {
    let manager = manager.clone();
    let ending = tokio::spawn(async move {
        (manager.runner)
            .stop_jobs_to_rerun(&*manager, JobStop::Sleep)
            .await;
    });
    if tokio::time::timeout_at(deadline, ending).await.is_err() {
        log!("sleep lock: CI jobs were still stopping as the machine went to sleep");
    }
}

/// The deadline of a hand-over before a sleep the platform holds until
/// `deadline`: [`HANDOVER_SHARE`] of the time left.
fn handover_deadline(deadline: Instant) -> Instant {
    let now = Instant::now();
    now + deadline
        .saturating_duration_since(now)
        .mul_f64(HANDOVER_SHARE)
}

/// Open the hub link and the agent queue again once the machine woke,
/// admitting work again after a hand-over before the sleep.
async fn woke(manager: &Manager) {
    log!("sleep lock: the machine woke");
    manager.hub.woke();
    manager.agents.agent_queue().sleep(false);
    manager.agents.come_back(manager).await;
}

#[cfg(test)]
mod tests;
