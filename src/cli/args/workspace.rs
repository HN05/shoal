use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum BaseCommand {
    /// Record the workspace whose branch this workspace builds on.
    Set {
        /// Workspace name or branch in the same repository; omit to choose one.
        base: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Remove the recorded base workspace.
    Clear {
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum WorkCommand {
    /// Show, set or clear the workspace a stacked workspace builds on.
    #[command(args_conflicts_with_subcommands = true)]
    Base {
        #[command(subcommand)]
        command: Option<BaseCommand>,
        workspace: Option<String>,
    },
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
    /// Rename a workspace's Git branch and derived workspace name.
    #[command(allow_missing_positional = true)]
    Rename {
        workspace: Option<String>,
        /// New literal Git branch name.
        branch: String,
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
