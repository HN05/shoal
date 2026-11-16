//! Arguments of commands that act on any space, a workspace or a chat.
use std::ffi::OsString;

use clap::Subcommand;

use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SpaceCommand {
    /// Open a workspace in a desktop app.
    ///
    /// The app runs untracked: Shoal gives it no instructions, scope or
    /// model, and cleanup does not see its activity.
    Open {
        workspace: Option<SpaceSelector>,
        /// The app to open the workspace in: claude (macOS), codex or t3;
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
