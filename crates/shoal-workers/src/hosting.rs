//! Where Shoal runs work for the caller's terminal: in it, or in the
//! background through a [`Host`](crate::terminal::Host).
mod add_place;
mod demos;
mod here;
mod launch;
mod naming;
mod session;
mod shell;

use std::io::IsTerminal;

pub use add_place::{Place, follows_caller, host_space, open_shell};
pub use demos::{demo_screen, open_demo, send_demo_keys, show_for_viewer, start_demo};
pub use here::{
    close_background_place_when_done, close_here_when_done, closes_with, label_current,
    record_owned_here,
};
pub use launch::{AgentSpace, opens_beside, run_agent, start_place};
pub use naming::{Labels, Renamed, relabel_space};
pub use session::{Session, run_session};
pub use shell::enter_directory;

use crate::{client::connection::CommandPaths, places::Launcher};

/// What hosting reads of the process it runs in: its paths, whether it
/// prints JSON, and the place a host opened for it, when it runs in one.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub paths: &'a CommandPaths,
    pub json: bool,
    pub place: Option<&'a Place>,
}

impl Caller<'_> {
    /// Whether a user is on this process's terminal, on both stdin and
    /// stderr, whatever output it prints: where it starts work, beside this
    /// terminal or in it.
    pub fn on_terminal() -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    /// What starts the work this process opens places for beside its
    /// terminal. A place a host opened for this process, or a terminal
    /// without a user, keeps work in place; a call that asked for JSON
    /// keeps its terminal's host.
    pub fn launcher(&self) -> Launcher {
        match self.place.is_none() && Self::on_terminal() {
            true => Launcher::Terminal,
            false => Launcher::Plain,
        }
    }
}
