//! Hold off a system shutdown until tracked work is stopped. The daemon holds
//! the login manager's shutdown delay lock while it runs; once a shutdown
//! starts, it stops what `shoal stop --all` stops while the wrappers can still
//! save it, then releases the lock so the shutdown continues. Without a login
//! manager the signals of the shutdown itself stop the work.
use std::sync::Arc;

use anyhow::Result;

use super::{log, notifications::NotificationKind, workspace::Manager};

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
        manager.stop_for_system_shutdown().await;
        drop(lock);
        if !announced(&mut login, false).await {
            return;
        }
        manager.system_shutdown_cancelled();
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
                .notify(None, NotificationKind::EnvironmentProblem, message)
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
