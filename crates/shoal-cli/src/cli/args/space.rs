//! Arguments of commands that act on any space, a workspace or a chat.
use std::ffi::OsString;

use clap::{Args, Subcommand};

use crate::cli::list_status;
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
    /// List what needs you: threads, approvals, agents needing input, failures and ready work.
    Inbox {
        /// List only this registered repository's items.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(short = 'p', long)]
        print: bool,
    },
    /// Pick a space from the ongoing work, or print the list.
    List {
        /// List only the running chats.
        #[arg(long)]
        chats: bool,
        /// List only this registered repository's workspaces, chats and agents.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
        /// List only workspaces, chats and agents with this status; repeat or separate with commas.
        #[arg(long, value_delimiter = ',')]
        status: Vec<list_status::ListStatus>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(short = 'p', long)]
        print: bool,
    },
    /// Show a space's activity and issue and PR state, and a workspace's
    /// changes and resources, or Shoal's status outside a space.
    ///
    /// Name a workspace or chat, find the spaces that link an issue, PR or
    /// CI run, or the workspaces that hold a resource: `status pr 12`,
    /// `status ci 2765`, `status <issue-PR-or-run-URL>`,
    /// `status resource <pool-or-member>`. Without a target, shows the space
    /// this runs in, else the daemon: its PID, version and unread
    /// notifications, stalled file access and failing cleanup; it exits 1
    /// when the daemon is not running.
    Status {
        /// Workspace or chat, issue, PR or CI run URL, or pr, issue, ci or
        /// resource.
        #[arg(value_name = "TARGET")]
        space: Option<String>,
        /// Number, URL, or resource name after pr, issue, ci or resource.
        item: Option<String>,
    },
    /// Show a space's log: its creation, agent, work, issues and PRs,
    /// threads, approvals and alerts, and the spaces below it; or Shoal's
    /// log outside a space.
    ///
    /// Without a space, shows the log of the space this runs in, else
    /// Shoal's: every space's entries, each naming its space, with the
    /// daemon's starts and stops, config reloads and cleanup runs. A removed
    /// space is selected by its ID while its log lasts. Entries print
    /// oldest first, by local time.
    Log(LogArgs),
    /// Give a space a new name, which lists and the tabs Shoal opens for it
    /// show.
    ///
    /// Without a space, renames the space this runs in, else offers a
    /// picker. Without arguments, picks the space and asks for its name.
    /// With --request, asks the space's running agent to choose a name that
    /// fits the work and set it. The worktree directory, branch and zmx
    /// session keep their names; `shoal branch rename` renames a
    /// workspace's Git branch instead.
    #[command(allow_missing_positional = true)]
    Rename {
        /// The space's number or ID.
        space: Option<SpaceSelector>,
        /// Free text of 1–40 characters on one line; names may repeat.
        name: Option<String>,
        /// Ask the space's agent to choose the name; a single argument then
        /// names the space.
        #[arg(long, conflicts_with = "space")]
        request: bool,
    },
}

#[derive(Debug, Args)]
pub struct LogArgs {
    pub space: Option<SpaceSelector>,
    /// Show every event Shoal recorded, such as agent turn states,
    /// forge comments and checks, demos and attached files.
    #[arg(short = 'A', long)]
    pub all: bool,
    /// Show the newest this many entries.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    /// Keep printing new entries as Shoal records them.
    #[arg(long)]
    pub follow: bool,
}

pub use crate::config::repo::DesktopApp;
