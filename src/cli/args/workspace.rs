//! Arguments of commands that list, enter, inspect and manage workspaces.
use std::{ffi::OsString, path::PathBuf};

use clap::Subcommand;

use crate::cli::{args::item_reference, list_status};
use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkspaceCommand {
    /// List what needs you: questions, approvals, agents needing input, failures and ready work.
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
        space: Option<SpaceSelector>,
        /// Number, URL, or resource name after pr, issue, ci or resource.
        #[arg(value_parser = item_reference)]
        item: Option<String>,
    },
    /// Show a space's log: its creation, agent, work, issues and PRs,
    /// questions, approvals and alerts, and the spaces below it; or Shoal's
    /// log outside a space.
    ///
    /// Without a space, shows the log of the space this runs in, else
    /// Shoal's: every space's entries, each naming its space, with the
    /// daemon's starts and stops, config reloads and cleanup runs. A removed
    /// space is named by its last name while its log lasts. Entries print
    /// oldest first, by local time.
    Log {
        space: Option<SpaceSelector>,
        /// Show every event Shoal recorded, such as agent turn states,
        /// forge comments and checks, demos and attached files.
        #[arg(short = 'A', long)]
        all: bool,
        /// Show the newest this many entries.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        /// Keep printing new entries as Shoal records them.
        #[arg(long)]
        follow: bool,
    },
    /// Enter a workspace, or use - for the one you were in before.
    ///
    /// Omit the workspace to open the picker, even inside a workspace.
    Cd { workspace: Option<SpaceSelector> },
    /// Open a workspace in an editor.
    ///
    /// Runs the configured `edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace path.
    Edit {
        workspace: Option<SpaceSelector>,
        /// Arguments forwarded to the editor.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
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
    /// Give a workspace a short title, which the tabs Shoal opens for it
    /// show, or a chat a new name.
    ///
    /// Without a space, titles the current workspace, else renames the chat
    /// this runs in, else offers a picker. Without arguments, picks the space
    /// and asks for its title or name. With --request, asks the space's
    /// running agent to choose a title or name that fits the work and set
    /// it. A chat's zmx session keeps its name; `shoal attach` finds it by
    /// the new one. `shoal branch rename` renames a workspace's Git branch
    /// instead.
    #[command(allow_missing_positional = true)]
    Rename {
        space: Option<SpaceSelector>,
        /// A workspace's title of at most 40 characters on one line, or a
        /// chat's name, free among workspaces and running chats.
        title: Option<String>,
        /// Ask the space's agent to choose the title or name; a single
        /// argument then names the space.
        #[arg(long, conflicts_with = "space")]
        request: bool,
    },
    /// Bring an existing worktree under Shoal management, including cleanup.
    Adopt {
        /// Existing worktree root; files and Git settings are preserved, setup is skipped.
        path: PathBuf,
        /// Registered repository; defaults to the one whose checkout or origin remote the worktree shares.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
        /// Copy the work into a new worktree under the repository's Shoal directory.
        #[arg(long)]
        copy: bool,
    },
    /// Run or retry workspace setup and the post-setup hook.
    Setup {
        /// Workspace to use; defaults to the current workspace or picker.
        workspace: Option<SpaceSelector>,
        /// Accept a failed setup as it is and mark the verified workspace ready.
        #[arg(long)]
        skip: bool,
    },
    /// Keep a workspace's HEAD and unsaved files in a checkpoint that shoal restore returns to.
    Checkpoint {
        workspace: Option<SpaceSelector>,
        /// Describe the checkpoint in shoal restore's list, in at most 200 characters.
        #[arg(long)]
        name: Option<String>,
    },
    /// Return a workspace to a checkpoint, or open a removed workspace again with its unsaved files and links.
    ///
    /// Without a workspace, picks a removed workspace to open again. With an existing workspace,
    /// picks one of its checkpoints; with a checkpoint number as well, returns to it after
    /// keeping the current state in a new checkpoint. Without a terminal, the choices are listed.
    Restore {
        workspace: Option<SpaceSelector>,
        #[arg(requires = "workspace")]
        checkpoint: Option<CheckpointNumber>,
    },
}

pub use crate::config::repo::DesktopApp;
