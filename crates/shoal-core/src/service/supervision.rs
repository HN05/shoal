//! How every process the `shoal` binary runs supervises its daemon
//! connections.
use crate::{
    client::{connection::Supervision, paths::Paths, processes::Identity},
    daemon_owner,
};

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
}
