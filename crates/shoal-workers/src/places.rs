//! The places Shoal opens for work the daemon queued or a command starts
//! beside its caller, the host that starts it there, and their names and
//! groups, with the repositories, issues and records they name read from
//! the daemon.
pub mod groups;
pub mod naming;
pub mod queued;

use crate::{
    config::Effective,
    terminal::{Configured, current},
};

/// What starts the work a command opens places for beside its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    /// The host serving the caller's terminal, which a user is on and no
    /// host opened for the caller's process.
    Terminal,
    /// A plain host, which keeps work in place: the caller runs in a place
    /// a host opened for it, or no user is on its terminal.
    Plain,
}

impl Launcher {
    /// The host that starts work beside the caller under `settings`.
    pub fn host(self, settings: &Effective) -> Configured {
        match self {
            Self::Terminal => current(settings),
            Self::Plain => Configured::plain(settings),
        }
    }
}
