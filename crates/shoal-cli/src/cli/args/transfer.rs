//! Arguments of `shoal move`.
use std::path::PathBuf;

use clap::Args;

use crate::cli::args::ConfirmationArgs;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

// Arguments of `shoal move`.
#[derive(Debug, Args)]
pub struct MoveArgs {
    #[arg(conflicts_with = "all", value_name = "SPACE")]
    pub space: Vec<SpaceSelector>,
    /// Move every workspace and chat of the repository --repo names.
    #[arg(short = 'A', long, requires = "repository")]
    pub all: bool,
    /// Repository whose spaces --all moves.
    #[arg(short = 'r', long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// Move to another machine's Shoal over SSH: a host, user@host or a ~/.ssh/config alias.
    #[arg(long, value_name = "HOST", conflicts_with = "output")]
    pub ssh: Option<String>,
    /// Write to this file for shoal restore --from, to a directory, which several spaces and
    /// --all need, a file per space, or with - to standard output.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Leave the spaces running here and move copies of them and of the spaces below them,
    /// which open as new spaces: a workspace at a new checkpoint on a branch of its own, or a
    /// chat whose agent starts from a fork of its session.
    #[arg(long)]
    pub keep: bool,
    /// Copy the workspace at this earlier checkpoint, numbered as shoal restore --checkpoint
    /// lists them.
    #[arg(long, requires = "keep", conflicts_with = "all", value_name = "N")]
    pub checkpoint: Option<CheckpointNumber>,
    /// Move the spaces below each space with it, the default.
    #[arg(long, overrides_with = "no_recursive")]
    pub recursive: bool,
    /// Leave the spaces below each space here, one level up, except those named too.
    #[arg(long, overrides_with = "recursive")]
    pub no_recursive: bool,
    /// Leave the agents stopped on the other machine for a later shoal restore instead of
    /// resuming them.
    #[arg(long, conflicts_with = "output")]
    pub no_resume: bool,
    /// The shoal program on the other machine, when SSH's PATH does not find it.
    #[arg(long, value_name = "PATH", conflicts_with = "output")]
    pub shoal: Option<String>,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

impl MoveArgs {
    /// Whether the spaces below each space go with it: as `--recursive` or
    /// `--no-recursive` says, else as the destination does by `default`.
    pub fn recursive(&self, default: bool) -> bool {
        match (self.recursive, self.no_recursive) {
            (true, _) => true,
            (_, true) => false,
            _ => default,
        }
    }
}
