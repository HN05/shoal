//! Read whether the user walked away from this machine, while its
//! `keep_awake` admits nothing new then. The agents learn of it at each
//! reading, and of the user's return once the lid opens or input returns.
use std::{sync::Arc, time::Duration};

use shoal_power::presence::{Platform, away, connect};

use super::{log, manager::Manager};

/// How often the platform is read while `keep_awake` acts on the user's
/// absence.
const INTERVAL: Duration = Duration::from_secs(5);

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
