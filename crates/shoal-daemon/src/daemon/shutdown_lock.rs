//! Hold off a system shutdown until tracked work is stopped. The daemon holds
//! the login manager's shutdown delay lock while it runs; once a shutdown
//! starts, it stops what `shoal stop --all` stops while the wrappers can still
//! save it and hands the spaces over within the lock's delay limit, then
//! releases the lock so the shutdown continues. Without a login manager the
//! signals of the shutdown itself stop the work.
use std::{sync::Arc, time::Duration};

use anyhow::Result;

use super::{log, manager::Manager, notifications::AlertKind};

#[cfg(target_os = "linux")]
mod logind;
#[cfg(not(target_os = "linux"))]
mod unsupported;

#[cfg(target_os = "linux")]
use logind::connect;
#[cfg(not(target_os = "linux"))]
use unsupported::connect;

/// The login manager's shutdown announcements and delay lock.
pub(super) trait LoginManager {
    /// Holds the shutdown off until dropped, or until the login manager's
    /// delay limit passes.
    type Lock;

    async fn delay_shutdown(&self) -> Result<Self::Lock>;

    /// How long the login manager holds a shutdown off for a delay lock.
    async fn delay_limit(&self) -> Duration;

    /// The next announcement, `true` as a shutdown starts and `false` when
    /// one is cancelled; `None` once announcements end.
    async fn announcement(&mut self) -> Option<bool>;
}

pub(super) async fn run(manager: Arc<Manager>) {
    if let Some(login) = connect().await {
        hold(&manager, login).await;
        log!("shutdown lock: the login manager stopped announcing shutdowns");
    }
}

/// Delay each shutdown until tracked work is stopped, and delay the next
/// one once a shutdown is cancelled, until announcements end.
async fn hold(manager: &Manager, mut login: impl LoginManager) {
    loop {
        let lock = delay_shutdown(manager, &login).await;
        if !announced(&mut login, true).await {
            return;
        }
        let window = login.delay_limit().await;
        (manager.agents)
            .stop_for_system_shutdown(manager, window)
            .await;
        drop(lock);
        if !announced(&mut login, false).await {
            return;
        }
        manager.agents.stay(manager).await;
    }
}

/// Take the delay lock, telling the user when the login manager refuses it,
/// since shutdowns then stop work only through their signals.
async fn delay_shutdown<L: LoginManager>(manager: &Manager, login: &L) -> Option<L::Lock> {
    match login.delay_shutdown().await {
        Ok(lock) => Some(lock),
        Err(error) => {
            log!("shutdown lock: {error:#}");
            let message =
                format!("Shoal could not delay system shutdowns to stop agents first: {error:#}");
            manager
                .coordination
                .notify(manager, None, AlertKind::EnvironmentProblem, message)
                .await;
            None
        }
    }
}

/// Wait for the announcement `starting`; false if announcements end first.
async fn announced(login: &mut impl LoginManager, starting: bool) -> bool {
    loop {
        match login.announcement().await {
            Some(announced) if announced == starting => return true,
            Some(_) => {}
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests;
