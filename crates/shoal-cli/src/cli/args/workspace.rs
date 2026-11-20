//! Arguments of commands that enter and manage workspaces, which need a
//! worktree.
use std::{ffi::OsString, path::PathBuf};

use clap::Subcommand;

use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkspaceCommand {
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
    ///
    /// Creates the worktree first when creating it failed. The agent waiting for a failed setup
    /// starts as the agent queue admits it once setup succeeds.
    Setup {
        /// Workspace to use; defaults to the current workspace or picker.
        workspace: Option<SpaceSelector>,
        /// Accept a failed setup as it is and mark the verified workspace ready.
        #[arg(long)]
        skip: bool,
    },
    /// Bring a workspace up to date with what it was made from, while it
    /// holds nothing of its own.
    ///
    /// A branch workspace fast-forwards to its upstream, else to the branch
    /// it builds on: its base workspace's, else the default branch, synced
    /// first. A workspace at a commit made for a PR or a branch moves to its
    /// head or tip; one at a tag or bare commit is up to date. A copy of a
    /// live workspace takes its source's files again and reruns setup.
    /// Commits the target lacks and uncommitted changes refuse it. Without a
    /// workspace, refreshes the one this runs in, else offers a picker.
    Refresh { workspace: Option<SpaceSelector> },
    /// Keep a workspace's HEAD and unsaved files in a checkpoint that shoal restore returns to.
    Checkpoint {
        workspace: Option<SpaceSelector>,
        /// Describe the checkpoint in shoal restore's list, in at most 200 characters.
        #[arg(long)]
        name: Option<String>,
    },
}
