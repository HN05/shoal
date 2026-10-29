//! Arguments of commands that list, enter, inspect and manage workspaces.
use std::{ffi::OsString, path::PathBuf};

use clap::Subcommand;

use crate::cli::{args::item_reference, list_status};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkspaceCommand {
    /// List what needs you: questions, approvals, agents needing input, failures and ready work.
    Inbox {
        /// List only this registered repository's items.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(long)]
        print: bool,
    },
    /// List ongoing work, or manage a workspace.
    #[command(args_conflicts_with_subcommands = true)]
    Work {
        #[command(subcommand)]
        command: Option<WorkCommand>,
        /// List only the running chats.
        #[arg(long)]
        chats: bool,
        /// List only this registered repository's workspaces, chats and agents.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// List only workspaces, chats and agents with this status; repeat or separate with commas.
        #[arg(long, value_delimiter = ',')]
        status: Vec<list_status::ListStatus>,
        /// Print the list instead of opening the picker, as filters and JSON do.
        #[arg(long)]
        print: bool,
    },
    /// Show workspace activity, changes, resources, and issue and PR state.
    ///
    /// Name a workspace, or find the workspaces that link an issue or PR or
    /// hold a resource: `status pr 12`, `status <issue-or-PR-URL>`, `status
    /// resource <pool-or-member>`.
    Status {
        /// Workspace, issue or PR URL, or pr, issue or resource.
        #[arg(value_name = "TARGET")]
        workspace: Option<String>,
        /// Number, URL, or resource name after pr, issue or resource.
        #[arg(value_parser = item_reference)]
        item: Option<String>,
    },
    /// Enter a workspace, or use - for the one you were in before.
    ///
    /// Omit the workspace to open the picker, even inside a workspace.
    Cd { workspace: Option<String> },
    /// Open a workspace in an editor.
    ///
    /// Runs the configured `edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace path.
    Edit {
        workspace: Option<String>,
        /// Arguments forwarded to the editor.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkCommand {
    /// Export the environment for processes started in a workspace.
    Env {
        workspace: Option<String>,
        /// Revoke a previously exported scope token.
        #[arg(long)]
        revoke: Option<String>,
    },
    /// Bring an existing worktree under Shoal management, including cleanup.
    Adopt {
        /// Existing worktree root; files and Git settings are preserved, setup is skipped.
        path: PathBuf,
        /// Registered repository; defaults to the one whose checkout or origin remote the worktree shares.
        #[arg(long = "repo")]
        repository: Option<String>,
        /// Copy the work into a new worktree under the repository's Shoal directory.
        #[arg(long)]
        copy: bool,
    },
    /// Give a workspace a short title, which the tabs Shoal opens for it show.
    ///
    /// `shoal branch rename` renames its Git branch instead.
    #[command(allow_missing_positional = true)]
    Rename {
        workspace: Option<String>,
        /// Title of at most 40 characters on one line.
        title: String,
    },
    /// Keep a workspace's HEAD and unsaved files in a checkpoint that shoal work restore returns to.
    Checkpoint {
        workspace: Option<String>,
        /// Describe the checkpoint in shoal work restore's list, in at most 200 characters.
        #[arg(long)]
        name: Option<String>,
    },
    /// Return a workspace to a checkpoint, or open a removed workspace again with its unsaved files and links.
    ///
    /// Without a workspace, picks a removed workspace to open again. With an existing workspace,
    /// picks one of its checkpoints; with a checkpoint number as well, returns to it after
    /// keeping the current state in a new checkpoint. Without a terminal, the choices are listed.
    Restore {
        workspace: Option<String>,
        #[arg(requires = "workspace")]
        checkpoint: Option<u64>,
    },
    /// Run or retry workspace setup and the post-setup hook.
    Setup {
        /// Workspace to use; defaults to the current workspace or picker.
        workspace: Option<String>,
        /// Accept a failed setup as it is and mark the verified workspace ready.
        #[arg(long)]
        skip: bool,
    },
}
