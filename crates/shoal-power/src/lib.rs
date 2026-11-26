//! The machine's power and the user's presence as the platform reports
//! them. What the daemon does on each stays with the daemon.
#[cfg(target_os = "linux")]
mod logind;
pub mod presence;
pub mod shutdown;
pub mod sleep;
