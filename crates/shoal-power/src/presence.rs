//! Whether the user walked away from this machine.
use anyhow::Result;

#[cfg(target_os = "linux")]
mod logind;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod readings;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
pub use logind::connect;
#[cfg(target_os = "macos")]
pub use macos::connect;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use unsupported::connect;

/// A machine's lid, as the platform reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lid {
    pub closed: bool,
    /// The machine runs with an external display or in a dock, so its user
    /// may work at it with the lid closed.
    pub docked: bool,
}

/// What the platform says of the user's presence.
#[allow(
    async_fn_in_trait,
    reason = "the daemon awaits it in the task that reads the platform"
)]
pub trait Platform {
    /// The machine's lid, none without one.
    async fn lid(&mut self) -> Result<Option<Lid>>;

    /// Whether the user has given no input as long as the platform reads
    /// as idle.
    async fn idle(&mut self) -> Result<bool>;
}

/// Whether the user walked away from the machine `platform` reads: its
/// lid closed, unless docked, where as without a lid, it is idle.
pub async fn away(platform: &mut impl Platform) -> Result<bool> {
    match platform.lid().await? {
        Some(Lid { closed: false, .. }) => Ok(false),
        Some(Lid { docked: false, .. }) => Ok(true),
        Some(Lid { docked: true, .. }) | None => platform.idle().await,
    }
}

#[cfg(test)]
mod tests;
