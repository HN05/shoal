use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::cli::args::ConfirmationArgs;

// Arguments of `shoal export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(conflicts_with = "all", value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// Export every workspace of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose workspaces --all exports.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// File to write, or a directory to write a file per workspace in, which several
    /// workspaces and --all need.
    #[arg(short, long)]
    pub output: PathBuf,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

// Arguments of `shoal import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Export files or directories of them.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Repository to import into, when it does not share the exported repository's origin.
    #[arg(long = "repo")]
    pub repository: Option<String>,
    /// Continue the stopped agents' sessions on any machine, such as one the agents'
    /// sessions were copied to.
    #[arg(long, conflicts_with = "new_sessions")]
    pub keep_sessions: bool,
    /// Start new sessions even on the machine that exported the workspace.
    #[arg(long, conflicts_with = "keep_sessions")]
    pub new_sessions: bool,
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
