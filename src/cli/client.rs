//! The CLI's connection to the daemon, which [`crate::client::connection`]
//! makes, supervised as the `shoal` command supervises its connections.
pub(crate) use crate::client::connection::*;
use crate::{
    client::{connection::Supervision, paths::Paths, processes::Identity},
    daemon::ownership,
};

/// The `shoal` command's [`Supervision`].
pub(crate) struct Command;

impl Supervision for Command {
    fn owner(&self, paths: &Paths) -> Option<Identity> {
        ownership::owner(paths)
    }

    fn managed(&self, paths: &Paths) -> bool {
        crate::service::installed(paths)
    }

    fn waiting_for_update(&self) {
        eprintln!("Waiting for the daemon to restart with the installed update...");
    }
}
