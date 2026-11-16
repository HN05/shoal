//! Arguments of commands that act on any space, a workspace or a chat.
use std::ffi::OsString;

use clap::Subcommand;

use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SpaceCommand {
    /// Open a space in a desktop app, in the directory its agent works in.
    ///
    /// A workspace opens in its worktree, a chat in the worktree or
    /// checkout it runs in, or in root_dir for a chat without a repository.
    /// The app runs untracked: Shoal gives it no instructions, scope or
    /// mode, and cleanup does not see its activity.
    Open {
        space: Option<SpaceSelector>,
        /// The app to open the space in: claude (macOS), codex or t3;
        /// defaults to app.default, else the only one installed, else a
        /// picker.
        #[arg(long, value_enum)]
        app: Option<DesktopApp>,
        /// Arguments forwarded to the app's launcher.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

pub use crate::config::repo::DesktopApp;
