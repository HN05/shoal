//! Read whether the user walked away from this machine, while its
//! `keep_awake` admits nothing new then: its lid is closed, or, on a
//! machine without a lid or docked with its lid closed, as with an external
//! display, it has gone without the user's input as long as the platform
//! reads as idle. The agents learn of it at each reading, and of the user's
//! return once the lid opens or input returns.
use std::{sync::Arc, time::Duration};

use anyhow::Result;

use super::{log, manager::Manager};

#[cfg(target_os = "linux")]
mod logind;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod readings;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
use logind::connect;
#[cfg(target_os = "macos")]
use macos::connect;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use unsupported::connect;

/// How often the platform is read while `keep_awake` acts on the user's
/// absence.
const INTERVAL: Duration = Duration::from_secs(5);

/// A machine's lid, as the platform reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Lid {
    pub closed: bool,
    /// The machine runs with an external display or in a dock, so its user
    /// may work at it with the lid closed.
    pub docked: bool,
}

/// What the platform says of the user's presence.
pub(super) trait Platform {
    /// The machine's lid, none without one.
    async fn lid(&mut self) -> Result<Option<Lid>>;

    /// Whether the user has given no input as long as the platform reads
    /// as idle.
    async fn idle(&mut self) -> Result<bool>;
}

/// Whether the user walked away from the machine `platform` reads: its
/// lid closed, unless docked, where as without a lid, it is idle.
async fn away(platform: &mut impl Platform) -> Result<bool> {
    match platform.lid().await? {
        Some(Lid { closed: false, .. }) => Ok(false),
        Some(Lid { docked: false, .. }) => Ok(true),
        Some(Lid { docked: true, .. }) | None => platform.idle().await,
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    if let Some(platform) = connect().await {
        follow(&manager, platform).await;
    }
}

/// Tell the agents at each reading whether the user is away, while
/// `keep_awake` acts on it, and that they are there otherwise.
async fn follow(manager: &Manager, mut platform: impl Platform) {
    let mut failure = None;
    loop {
        let acts = manager.config().machine.keep_awake.stops_when_away();
        let away = acts && walked_away(&mut platform, &mut failure).await;
        manager.agents.user_away(manager, away).await;
        tokio::time::sleep(INTERVAL).await;
    }
}

/// Whether the user walked away as `platform` reads it, as though they
/// are there when it cannot read it, logging a `failure` once until it
/// changes or a reading succeeds.
async fn walked_away(platform: &mut impl Platform, failure: &mut Option<String>) -> bool {
    match away(platform).await {
        Ok(away) => {
            *failure = None;
            away
        }
        Err(error) => {
            let error = format!("{error:#}");
            if failure.as_ref() != Some(&error) {
                log!("walked away: unreadable, so the user counts as present: {error}");
                *failure = Some(error);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests;
