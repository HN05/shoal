//! Arguments of commands that stop, remove and move workspaces.
use clap::{Args, Subcommand};

use crate::cli::args::{ConfirmationArgs, WorkspaceTargets, transfer::MoveCommand};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum LifecycleCommand {
    /// Stop tracked agents and commands, saving them for `shoal resume`; keep the workspaces.
    Stop {
        #[command(flatten)]
        targets: WorkspaceTargets,
        /// Treat executions Shoal cannot prove ended as stopped without asking.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Keep running the workspaces below the selected ones.
        #[arg(long, conflicts_with = "all")]
        skip_children: bool,
        /// Cancel the current turns of the agents instead, keeping them running.
        ///
        /// Shoal types each agent's interrupt key into its terminal session, records its turn
        /// as ended, and leaves agents without a running turn alone.
        #[arg(long, conflicts_with_all = ["all", "yes", "skip_children"])]
        interrupt: bool,
    },
    /// Remove workspaces and release their resources.
    ///
    /// A branch goes too when a remote or the default branch has all its commits; otherwise it
    /// stays. Deleting uncommitted changes or commits on a detached HEAD needs confirmation,
    /// asked once for every workspace.
    Rm {
        #[arg(value_name = "WORKSPACE")]
        workspace: Vec<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Remove what Shoal left behind: workspaces cleanup would remove, kept branches and caches.
    ///
    /// Without a kind, lists them; on a terminal, a picker offers them, where Tab selects several
    /// and nothing starts selected.
    Cleanup {
        #[command(subcommand)]
        kind: Option<CleanupKind>,
        #[command(flatten)]
        args: CleanupArgs,
    },
    /// Move workspaces to another machine, or repositories to another root directory.
    Move {
        #[command(subcommand)]
        command: MoveCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CleanupKind {
    /// Remove the workspaces idle or merged cleanup would remove, without waiting for their idle
    /// periods.
    ///
    /// Applies where those rules are disabled; held, busy, unpushed or unmerged dirty workspaces are
    /// kept, and the unsaved files of merged ones go into their checkpoints. Names limit it to those
    /// workspaces.
    Workspaces { names: Vec<String> },
    /// Delete branches Shoal kept when it removed their workspaces, with the commits only they have.
    ///
    /// Removal keeps a branch only while some of its commits are on no remote and not on the
    /// default branch, and deletes it once they are. Without names or --all, choose them in a
    /// picker, where Tab selects several.
    Branches {
        names: Vec<String>,
        /// Delete every kept branch.
        #[arg(long, conflicts_with = "names")]
        all: bool,
    },
    /// Empty the shared caches of repositories without running executions.
    Caches,
}

// Options of `shoal cleanup` that every kind takes.
#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// List what would be removed without removing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Only this repository's workspaces, kept branches and caches, by name or ID.
    #[arg(long, global = true)]
    pub repo: Option<String>,
    /// Delete kept branches without asking.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
}
