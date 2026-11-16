//! How every process the `shoal` binary runs supervises its daemon
//! connections.
use crate::{client::connection::Supervision, daemon_owner};
use shoal_protocol::{paths::Paths, processes::Identity};

/// The `shoal` binary's [`Supervision`].
pub struct Command;

impl Supervision for Command {
    fn owner(&self, paths: &Paths) -> Option<Identity> {
        daemon_owner::owner(paths)
    }

    fn managed(&self, paths: &Paths) -> bool {
        super::installed(paths)
    }

    fn waiting_for_update(&self) {
        eprintln!("Waiting for the daemon to restart with the installed update...");
    }

    fn waiting_for_restart(&self) {
        eprintln!("The daemon is exiting; waiting for it to start again...");
    }
}
