//! The platform's announcements of the machine's sleep.
use std::time::Duration;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "linux")]
mod logind;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use iokit::connect;
#[cfg(target_os = "linux")]
pub use logind::connect;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use unsupported::connect;

/// What the platform announces of the machine's sleep.
pub enum PowerEvent<H> {
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
#[expect(
    async_fn_in_trait,
    reason = "the daemon awaits it in the task that reads the platform"
)]
pub trait PowerEvents {
    /// Holds a coming sleep off until dropped.
    type Hold;

    /// The next announcement; none once announcements end.
    async fn next(&mut self) -> Option<PowerEvent<Self::Hold>>;
}
