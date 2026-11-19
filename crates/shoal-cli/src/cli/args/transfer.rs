//! Arguments of `shoal move`.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::cli::args::ConfirmationArgs;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum MoveCommand {
    /// End a workspace or chat here and write it to a file that shoal restore --from continues on another machine.
    ///
    /// A workspace is removed: the file holds its checkpoint, as a Git bundle without the
    /// commits of the default branch, its base, links and stopped agents. A chat's agent is
    /// stopped once it has reported its session: the file holds the chat's name and agent.
    /// Every space below it, and the chats in the worktree of each workspace among them, end
    /// with it, in its file. The file carries the files of the agents' sessions, such as their
    /// transcripts. A removed space is exported without another removal, with the spaces its
    /// removal took. Without -o, or with `-`, it writes to standard output, which must not be a
    /// terminal. When -o names a directory, several spaces, or with --all every space of the
    /// repository, go to a file each in it, those below another in its file, once each one can
    /// be exported. --no-children leaves the spaces below here instead, moved up one level.
    /// --keep leaves the spaces running and writes copies of them instead.
    Export(ExportArgs),
    /// Move workspaces and chats to another machine's Shoal over SSH, where their agents continue.
    ///
    /// Runs shoal move export | ssh HOST shoal restore --from - for each space, once ssh
    /// reaches Shoal there; nothing ends when it does not. The other machine needs a repository
    /// with the same origin. A workspace whose import fails stays removed here, and shoal
    /// restore opens it again; a chat's file stays here for shoal restore --from. --keep copies the
    /// spaces instead and leaves them running here.
    Ssh(SshArgs),
}

// Arguments of `shoal move export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(conflicts_with = "all", value_name = "SPACE")]
    pub space: Vec<SpaceSelector>,
    /// Export every workspace and chat of the repository --repo names.
    #[arg(short = 'A', long, requires = "repository")]
    pub all: bool,
    /// Repository whose spaces --all exports.
    #[arg(short = 'r', long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// File to write, or a directory to write a file per space in, which several spaces
    /// and --all need; defaults to standard output.
    #[arg(short, long, required_if_eq("all", "true"))]
    pub output: Option<PathBuf>,
    /// Leave the spaces running and export copies of them and of the spaces below them, which
    /// shoal restore --from opens as new spaces: a workspace at a new checkpoint on a branch of its
    /// own, or a chat whose agent starts from a fork of its session.
    #[arg(long)]
    pub keep: bool,
    /// Export a copy of the workspace at this earlier checkpoint, numbered as shoal restore
    /// --checkpoint lists them.
    #[arg(long, requires = "keep", conflicts_with = "all", value_name = "N")]
    pub checkpoint: Option<CheckpointNumber>,
    /// Leave the spaces below each space here, one level up, except those named too.
    #[arg(long)]
    pub no_children: bool,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

// Arguments of `shoal move ssh`.
#[derive(Debug, Args)]
pub struct SshArgs {
    /// SSH destination: a host, user@host or a ~/.ssh/config alias.
    pub host: String,
    #[arg(conflicts_with = "all", value_name = "SPACE")]
    pub space: Vec<SpaceSelector>,
    /// Move every workspace and chat of the repository --repo names.
    #[arg(short = 'A', long, requires = "repository")]
    pub all: bool,
    /// Repository whose spaces --all moves.
    #[arg(short = 'r', long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// Leave the spaces running here and copy them, which the other machine opens as new
    /// spaces: a workspace at a new checkpoint on a branch of its own, or a chat from a fork of
    /// its session.
    #[arg(long)]
    pub keep: bool,
    /// Copy the workspace at this earlier checkpoint, numbered as shoal restore --checkpoint lists them.
    #[arg(long, requires = "keep", conflicts_with = "all", value_name = "N")]
    pub checkpoint: Option<CheckpointNumber>,
    /// Leave the agents stopped on the other machine instead of queueing their restores.
    #[arg(long)]
    pub no_restore: bool,
    /// Leave the spaces below each space here, one level up, except those named too.
    #[arg(long)]
    pub no_children: bool,
    /// The shoal program on the other machine, when SSH's PATH does not find it.
    #[arg(long, value_name = "PATH", default_value = "shoal")]
    pub shoal: String,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}
