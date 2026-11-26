//! Stopping tracked work while the daemon and wrappers still run to save it,
//! before a shutdown, a walk-away or a pause, and handing spaces over to other
//! machines of the hub. `docs/design/power.md` and `docs/design/hub.md` say
//! when each applies.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use futures_util::future::join_all;
use shoal_protocol::hub::packages::{Handed, Handing, Leaving};
use tokio::time::Instant;

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery;
use crate::execution::recovery::Saved;
use crate::execution::recovery::SavedExt as _;
use crate::executions::StopRequest;
use crate::log;
use crate::model::Space;
use crate::protocol::QueuedLaunches;
use shoal_daemon_base::alerts::AlertKind;
use shoal_daemon_base::jobs::JobStop;
use shoal_daemon_base::notifications::About;
use shoal_protocol::space_names::SpaceSelector;

/// The spaces a stop of every running space stopped.
#[derive(Default)]
pub struct StoppedSpaces {
    pub spaces: Vec<Space>,
    /// Whether it stopped every running space's work there; otherwise work
    /// may also stop elsewhere, as the daemon's last stop of what still
    /// runs as it exits saves it.
    pub every: bool,
}

impl StoppedSpaces {
    /// A stop that stopped no work anywhere.
    pub fn none() -> Self {
        Self {
            spaces: Vec::new(),
            every: true,
        }
    }
}

/// What ends this machine's work, which the stop and the user's
/// notifications name.
#[derive(Clone, Copy)]
pub enum Shutdown {
    System,
    /// A signal ends the daemon, as a logout or a service manager's stop sends.
    Daemon,
    /// The user walked away under `keep_awake = "handover"`.
    Away,
    /// The machine goes to sleep under `keep_awake = "handover"`.
    Sleep,
    /// The user paused the machine.
    Pause,
}

impl Shutdown {
    fn request(self) -> StopRequest {
        StopRequest::shutdown(match self {
            Self::System => "the system is shutting down",
            Self::Daemon => "the Shoal daemon is stopping",
            Self::Away => "the user walked away from this machine",
            Self::Sleep => "this machine is going to sleep",
            Self::Pause => "the user paused this machine",
        })
    }

    /// When the work stopped, as the user's notifications say it.
    fn moment(self) -> &'static str {
        match self {
            Self::System => "before the system shut down",
            Self::Daemon => "before the Shoal daemon stopped",
            Self::Away => "once you walked away from this machine",
            Self::Sleep => "before this machine slept",
            Self::Pause => "as you paused this machine",
        }
    }

    fn notification(self) -> AlertKind {
        match self {
            Self::System => AlertKind::SystemShutdown,
            Self::Daemon => AlertKind::DaemonStopped,
            Self::Away | Self::Sleep | Self::Pause => AlertKind::HandedOver,
        }
    }

    /// Whether the machine's work ends with the stop, so a later one leaves
    /// it alone: as the system or the daemon shuts down, not as the user
    /// walks away or pauses the machine, or it sleeps.
    fn ends_work(self) -> bool {
        matches!(self, Self::System | Self::Daemon)
    }

    /// Whether the stop stops every space's agents, not only those of the
    /// spaces it hands over: all but a walk-away's or a sleep's, which
    /// leave the rest to the sleep.
    fn stops_every_space(self) -> bool {
        matches!(self, Self::System | Self::Daemon | Self::Pause)
    }

    /// Why the stop also ends CI jobs, to rerun, if it does: a system
    /// shutdown and a pause end them, while the daemon's own end leaves
    /// them to the next daemon, since the forge's runner does their work,
    /// and a machine its user left runs them until it sleeps.
    fn ends_jobs(self) -> Option<JobStop> {
        match self {
            Self::System => Some(JobStop::SystemShutdown),
            Self::Pause => Some(JobStop::Pause),
            Self::Daemon | Self::Away | Self::Sleep => None,
        }
    }

    /// When the work that stays here continues, after how it does: once a
    /// pause ends, or as soon as it can otherwise.
    fn resumes_when(self) -> &'static str {
        match self {
            Self::Pause => " once shoal unpause admits work again",
            Self::System | Self::Daemon | Self::Away | Self::Sleep => "",
        }
    }

    /// Why the machine hands its spaces over.
    fn leaving(self) -> Leaving {
        match self {
            Self::Pause => Leaving::Paused,
            Self::System | Self::Daemon | Self::Away | Self::Sleep => Leaving::Away,
        }
    }
}

impl Agents {
    /// Stop the spaces `shoal stop --all` stops before the system shuts
    /// down, and hand them over within the shutdown's `window`. See
    /// [`Self::stop_for_shutdown_of`].
    pub async fn stop_for_system_shutdown(&self, daemon: &dyn Daemon, window: Duration) {
        let window = HandoverWindow::Shared(window);
        (self.stop_for_shutdown_of(daemon, Shutdown::System, window)).await;
    }

    /// Stop the spaces `shoal stop --all` stops, each wrapper saving its
    /// agent before the stop and requesting restores that need no user,
    /// which the stop keeps with those already waiting, and leaving CI jobs
    /// running unless `shutdown` ends them, each alone, to rerun. Hold the
    /// agent queue until a system shutdown is cancelled, so nothing starts
    /// again before it.
    /// Then, on a machine joined to a hub, hand every space over to the
    /// machines the hub places them on, sending what fits in `window`, but
    /// those an earlier hand-over sent, which run elsewhere already. The
    /// user learns where each space went and what to continue. An update
    /// waits for the stop. Once a stop before a shutdown finished, later
    /// ones wait for it and leave the work alone; a hand-over once the user
    /// walked away or before a sleep leaves a later shutdown its stop, and
    /// one that comes during its hand-over.
    /// Returns the spaces stopped.
    pub async fn stop_for_shutdown_of(
        &self,
        daemon: &dyn Daemon,
        shutdown: Shutdown,
        window: HandoverWindow,
    ) -> StoppedSpaces {
        let started = Instant::now();
        let finished = self.shutdown_stopped.lock().await;
        if *finished {
            return StoppedSpaces::default();
        }
        // A CI job's runner stops with its job, which ends apart.
        let left = daemon.job_workspaces().await;
        let handing = self.handing(daemon, shutdown).await;
        let stops = |space: &Space| {
            !left.contains(&space.id) && (shutdown.stops_every_space() || handing.takes(space))
        };
        let spaces = self.stop_every_target(daemon, shutdown.moment(), stops, |space| async move {
            self.stop_keeping_restores(daemon, &SpaceSelector::id(&space), shutdown.request())
                .await
        });
        let jobs = async {
            if let Some(why) = shutdown.ends_jobs() {
                daemon.stop_jobs_to_rerun(why).await;
            }
        };
        let (stopped, ()) = tokio::join!(spaces, jobs);
        // A shutdown during a hand-over once the user walked away or paused
        // the machine, or before a sleep, goes ahead, leaving alone the
        // spaces under way.
        let latch = shutdown.ends_work().then_some(finished);
        let deadline = window.deadline(started);
        let handed = (self.hand_over(daemon, deadline, handing, shutdown.leaving())).await;
        (self.notify_shutdown_stop(daemon, shutdown, &stopped.spaces, &handed)).await;
        if let Some(mut finished) = latch {
            *finished = true;
        }
        stopped
    }

    /// The spaces a stop before `shutdown` hands over, but those an
    /// earlier hand-over sent: every space, or once the user walked away
    /// or paused the machine, or before it sleeps, those with something to
    /// do, none when they cannot be read.
    async fn handing(&self, daemon: &dyn Daemon, shutdown: Shutdown) -> Handing {
        let gone = self.presences.gone();
        if shutdown.ends_work() {
            return Handing::AllBut(gone);
        }
        let spaces = self
            .spaces_to_hand_over(daemon)
            .await
            .unwrap_or_else(|error| {
                log!("hand-over: the spaces with something to do are unreadable: {error:#}");
                HashSet::new()
            });
        Handing::Only(spaces.difference(&gone).cloned().collect())
    }

    /// Admit queued agents again once the shutdown that stopped work was
    /// cancelled, and stop work again before the next one. The spaces
    /// handed over meanwhile run on other machines, so this one retires its
    /// copies.
    pub async fn stay(&self, daemon: &dyn Daemon) {
        *self.shutdown_stopped.lock().await = false;
        self.agent_queue.suspend(false).await;
        self.retire_handed_over(daemon).await;
    }

    /// [`Self::stay`] once the user who walked away came back, unless a
    /// shutdown's stop ran since, whose hold on the queue stands. Only the
    /// check and the queue's release hold the stop's latch, so a shutdown
    /// meanwhile does not wait for the hub.
    pub(crate) async fn stay_unless_shut_down(&self, daemon: &dyn Daemon) {
        {
            let finished = self.shutdown_stopped.lock().await;
            if *finished {
                return;
            }
            self.agent_queue.suspend(false).await;
        }
        self.retire_handed_over(daemon).await;
    }

    /// Have this machine retire its copies of the spaces handed over.
    async fn retire_handed_over(&self, daemon: &dyn Daemon) {
        daemon.stay().await;
        self.presences.forget_gone();
    }

    /// Stop each space `shoal stop --all` stops that `stops` takes, with
    /// `stop`, given its ID, in parallel, holding the agent queue so nothing
    /// starts again, and tell the user of each that failed to stop `moment`.
    /// Returns those stopped.
    pub async fn stop_every_target<F, Fut>(
        &self,
        daemon: &dyn Daemon,
        moment: &str,
        stops: impl Fn(&Space) -> bool,
        stop: F,
    ) -> StoppedSpaces
    where
        F: Fn(SpaceId) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _operation = self.gates.operation().await;
        self.agent_queue.suspend(true).await;
        let targets = match self.stop_targets(daemon, QueuedLaunches::Keep).await {
            Ok(targets) => targets,
            Err(error) => {
                log!("stop {moment}: running spaces unavailable: {error:#}");
                return StoppedSpaces::default();
            }
        };
        let targets: Vec<Space> = targets.into_iter().filter(|space| stops(space)).collect();
        let results = join_all(targets.iter().map(|space| stop(space.id.clone()))).await;
        let mut stopped = StoppedSpaces::none();
        for (space, result) in targets.into_iter().zip(results) {
            match result {
                Ok(()) => stopped.spaces.push(space),
                Err(error) => {
                    stopped.every = false;
                    self.notify_stop_failed(daemon, moment, &space, &error)
                        .await;
                }
            }
        }
        stopped
    }

    async fn notify_stop_failed(
        &self,
        daemon: &dyn Daemon,
        moment: &str,
        space: &Space,
        error: &anyhow::Error,
    ) {
        log!("stop {moment}: {} not stopped: {error:#}", &space.name);
        daemon
            .notify_about(
                About::space(&space.name, &space.id),
                AlertKind::StopFailed,
                format!("Could not stop tracked executions {moment}: {error:#}"),
            )
            .await;
    }

    /// Hand the spaces `handing` takes over through the hub by `deadline`
    /// as this machine is `leaving`, and where each went. A space that went
    /// keeps no restore here, so it does not come back at boot, where this
    /// machine retires it once the hub names it, as after any takeover.
    async fn hand_over(
        &self,
        daemon: &dyn Daemon,
        deadline: Instant,
        handing: Handing,
        leaving: Leaving,
    ) -> Vec<(Space, Handed)> {
        let under_way = handing.named();
        self.presences.leaving(&under_way);
        let handed = daemon.hand_over(deadline, handing, leaving).await;
        self.presences.settled(&under_way, went(&handed));
        for (space, handed) in &handed {
            if !handed.left() {
                continue;
            }
            if let Err(error) = self.cancel_stopped_restores(daemon, &space.id).await {
                log!(
                    "{} went to another machine, but its restores here stay: {error:#}",
                    space.name
                );
            }
        }
        handed
    }

    /// Tell the user what the stop of `stopped` saved, where the spaces
    /// `handed` over went, and how the work that stays here continues.
    async fn notify_shutdown_stop(
        &self,
        daemon: &dyn Daemon,
        shutdown: Shutdown,
        stopped: &[Space],
        handed: &[(Space, Handed)],
    ) {
        let moment = shutdown.moment();
        let stopped = self.stopped_report(shutdown, stopped, handed).await;
        let handed =
            (!handed.is_empty()).then(|| format!("Handed over {moment}: {}", handovers(handed)));
        let told: Vec<String> = stopped.into_iter().chain(handed).collect();
        if told.is_empty() {
            return;
        }
        let message = told.join(". ");
        (daemon.notify(None, shutdown.notification(), message)).await;
    }

    /// What the user reads of the work stopped in the `stopped` spaces
    /// before `shutdown`, and how the work of those that `handed` does not
    /// say went elsewhere continues here; none without any.
    async fn stopped_report(
        &self,
        shutdown: Shutdown,
        stopped: &[Space],
        handed: &[(Space, Handed)],
    ) -> Option<String> {
        if stopped.is_empty() {
            return None;
        }
        let count = stopped.len();
        let spaces = if count == 1 { "space" } else { "spaces" };
        let moment = shutdown.moment();
        let here = staying(stopped, handed);
        if here.is_empty() {
            return Some(format!("Stopped work in {count} {spaces} {moment}"));
        }
        let mut continues = continuation(&self.awaiting_restore(&here).await);
        continues.push_str(shutdown.resumes_when());
        Some(format!(
            "Stopped work in {count} {spaces} {moment}; {continues}"
        ))
    }

    /// Those of the `stopped` spaces whose saved work only `shoal resume`
    /// continues; one whose saved work is unreadable counts.
    pub async fn awaiting_restore<'a>(&self, stopped: &'a [Space]) -> Vec<&'a Space> {
        let queued = self
            .queued_stopped_restores()
            .await
            .unwrap_or_else(|error| {
                log!("shutdown stop: agent queue unavailable: {error:#}");
                HashSet::new()
            });
        (stopped.iter())
            .filter(|space| {
                waits_for_resume(&self.paths, space, &queued).unwrap_or_else(|error| {
                    log!(
                        "shutdown stop: stopped work of {} unreadable: {error:#}",
                        &space.name
                    );
                    true
                })
            })
            .collect()
    }
}

/// Whether the space saved work only `shoal resume` continues: a command,
/// or an agent or last session whose restore is neither `queued` nor
/// requested. A done space's requests wait for `shoal resume` to name it.
pub fn waits_for_resume(
    paths: &crate::paths::Paths,
    space: &Space,
    queued: &HashSet<ExecutionId>,
) -> Result<bool> {
    let saved = Saved::load(paths, &space.id)?;
    let requested: HashSet<ExecutionId> = if space.done() {
        HashSet::new()
    } else {
        recovery::restore_requests(paths, &space.id)?
            .into_iter()
            .map(|(execution, _)| execution)
            .collect()
    };
    Ok(!saved.commands.is_empty()
        || (saved.agents.iter().chain(&saved.sessions))
            .any(|agent| !requested.contains(&agent.id) && !queued.contains(&agent.id)))
}

/// How long the hand-over after a stop before a shutdown may send.
#[derive(Clone, Copy, Debug)]
pub enum HandoverWindow {
    /// What a window that began with the stop leaves, as logind's delay
    /// limit does, but a moment for the user's notification.
    Shared(Duration),
    /// This long once the stop ended, which the service manager's stop
    /// timeout leaves it besides the daemon's own drain.
    Own(Duration),
    /// Until this moment, by which the platform sleeps.
    Until(Instant),
}

impl HandoverWindow {
    /// The latest a hand-over that starts now, once the stop that
    /// `started` ended, may send.
    fn deadline(self, started: Instant) -> Instant {
        match self {
            Self::Shared(window) => started + window.saturating_sub(HANDOVER_MARGIN),
            Self::Own(budget) => Instant::now() + budget,
            Self::Until(deadline) => deadline,
        }
    }
}

/// What a stop before a shutdown keeps of its window after a hand-over.
const HANDOVER_MARGIN: Duration = Duration::from_secs(1);

/// Those of the `stopped` spaces that stay here, which `handed` does not
/// say this machine left to the hub.
fn staying(stopped: &[Space], handed: &[(Space, Handed)]) -> Vec<Space> {
    let went = went(handed);
    (stopped.iter())
        .filter(|space| !went.contains(&space.id))
        .cloned()
        .collect()
}

/// The spaces `handed` says this machine left to the hub.
fn went(handed: &[(Space, Handed)]) -> HashSet<SpaceId> {
    (handed.iter())
        .filter(|(_, handed)| handed.left())
        .map(|(space, _)| space.id.clone())
        .collect()
}

/// Where each space `handed` over went, or why it stays here.
fn handovers(handed: &[(Space, Handed)]) -> String {
    let each = handed.iter().map(|(space, handed)| {
        let (name, number) = (&space.name, space.number);
        match handed {
            Handed::Went(decision) => {
                format!("{name} ({number}) went to machine {}", decision.name)
            }
            Handed::Unconfirmed(decision) => format!(
                "{name} ({number}) may have gone to machine {}, which the hub did not confirm in time, as shoal list shows once this machine is back",
                decision.name
            ),
            Handed::Stayed(why) => format!("{name} ({number}) stays here: {why}"),
        }
    });
    each.collect::<Vec<_>>().join("; ")
}

/// How the work of the `waiting` spaces continues. `shoal resume --all`
/// skips done spaces, so they are named.
pub fn continuation(waiting: &[&Space]) -> String {
    let (done, open): (Vec<&Space>, Vec<&Space>) = waiting.iter().partition(|space| space.done());
    let mut commands = Vec::new();
    if !open.is_empty() {
        commands.push("shoal resume --all".to_owned());
    }
    if !done.is_empty() {
        let numbers: Vec<String> = done.iter().map(|space| space.number.to_string()).collect();
        commands.push(format!("shoal resume {}", numbers.join(" ")));
    }
    if commands.is_empty() {
        "the agent queue restores its agents".into()
    } else {
        format!("continue it with {}", commands.join(" and "))
    }
}

#[cfg(test)]
mod tests;
