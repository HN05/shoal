//! The machine's power and the user's presence as the platform reports
//! them: the announcements of a coming sleep or shutdown, with the delay
//! locks that hold them off until the daemon is ready, and whether the
//! user walked away from the machine. What the daemon does on each is its
//! own.
#[cfg(target_os = "linux")]
mod logind;
pub mod presence;
pub mod shutdown;
pub mod sleep;
