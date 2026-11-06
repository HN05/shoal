//! How every process the `shoal` binary runs supervises its daemon
//! connections.
use crate::{
    client::{connection::Supervision, paths::Paths, processes::Identity},
    daemon::ownership,
};

/// The `shoal` binary's [`Supervision`].
pub(crate) struct Command;

impl Supervision for Command {
    fn owner(&self, paths: &Paths) -> Option<Identity> {
        ownership::owner(paths)
    }

    fn managed(&self, paths: &Paths) -> bool {
        super::installed(paths)
    }

    fn waiting_for_update(&self) {
        eprintln!("Waiting for the daemon to restart with the installed update...");
    }
}
