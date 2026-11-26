//! The login manager's announcements of a system shutdown and the delay
//! lock that holds one off.
use std::time::Duration;

use anyhow::Result;

#[cfg(target_os = "linux")]
mod logind;
#[cfg(not(target_os = "linux"))]
mod unsupported;

#[cfg(target_os = "linux")]
pub use logind::connect;
#[cfg(not(target_os = "linux"))]
pub use unsupported::connect;

/// The login manager's shutdown announcements and delay lock.
#[allow(
    async_fn_in_trait,
    reason = "the daemon awaits it in the task that holds the lock"
)]
pub trait LoginManager {
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
