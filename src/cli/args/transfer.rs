//! Arguments of `shoal move`.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::cli::args::ConfirmationArgs;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum MoveCommand {
    /// End a workspace or chat here and write it to a file that shoal move import continues on another machine.
    ///
    /// A workspace is removed: the file holds its checkpoint, as a Git bundle without the
    /// commits of the default branch, its base, links and stopped agents. A chat's agent is
    /// stopped once it has reported its session: the file holds the chat's name and agent.
    /// Both carry the files of the agents' sessions, such as their transcripts. A removed
    /// workspace is exported without another removal. Without -o, or with `-`, it writes to
    /// standard output, which must not be a terminal. When -o names a directory, several spaces,
    /// or with --all every space of the repository, go to a file each in it, once each one can
    /// be exported. --keep leaves the space running and writes a copy of it instead.
    Export(ExportArgs),
    /// Continue workspaces and chats shoal move export wrote, in the repository with the same origin.
    ///
    /// It puts the agents' session files where the agents look for them; a session the file
    /// lacks continues only on the machine that exported it. --keep-sessions and --new-sessions
    /// override that. A workspace opens again, fetching the default branch when the file builds
    /// on commits the repository lacks, and shoal resume continues its agents. A chat starts in
    /// the repository's checkout, resuming its session: beside this terminal, or in the
    /// background with --resume or without a terminal. Without a file, or with `-`, it reads
    /// from standard input, which must not be a terminal. A directory imports each .shoal file
    /// in it.
    Import(ImportArgs),
    /// Move workspaces and chats to another machine's Shoal over SSH, where their agents continue.
    ///
    /// Runs shoal move export | ssh HOST shoal move import --resume for each space, once ssh
    /// reaches Shoal there; nothing ends when it does not. The other machine needs a repository
    /// with the same origin. A workspace whose import fails stays removed here, and shoal work
    /// restore opens it again; a chat's file stays here for shoal move import.
    Ssh(SshArgs),
    /// Move every repository directory, with its workspaces and clone, into root_dir.
    Root {
        /// New root directory, saved as root_dir first; omit to use the configured one.
        #[arg(value_hint = clap::ValueHint::DirPath)]
        root: Option<PathBuf>,
        /// Keep root_dir a regular directory instead of a link to one Spotlight skips (macOS).
        #[arg(long)]
        regular_dir: bool,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
}

// Arguments of `shoal move export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(conflicts_with = "all", value_name = "SPACE")]
    pub space: Vec<String>,
    /// Export every workspace and chat of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose spaces --all exports.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// File to write, or a directory to write a file per space in, which several spaces
    /// and --all need; defaults to standard output.
    #[arg(short, long, required_if_eq("all", "true"))]
    pub output: Option<PathBuf>,
    /// Leave the space running and export a copy of it, which shoal move import opens as a new
    /// space: a workspace at a new checkpoint on a branch of its own, or a chat from a fork of
    /// its session.
    #[arg(long, conflicts_with = "all")]
    pub keep: bool,
    /// Export a copy of the workspace at this earlier checkpoint, numbered as shoal work restore
    /// lists them.
    #[arg(long, requires = "keep", value_name = "N")]
    pub checkpoint: Option<u64>,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

// Arguments of `shoal move import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Export files or directories of them; defaults to standard input.
    pub files: Vec<PathBuf>,
    /// Repository to import into, when it does not share the exported repository's origin.
    #[arg(long = "repo")]
    pub repository: Option<String>,
    /// Continue every stopped agent's session, such as one whose files were copied by hand;
    /// refused for a copy, whose agents start from forks.
    #[arg(long, conflicts_with = "new_sessions")]
    pub keep_sessions: bool,
    /// Start new sessions even on the machine that exported the space.
    #[arg(long, conflicts_with = "keep_sessions")]
    pub new_sessions: bool,
    /// Queue the restores of the agents the export stopped, as shoal resume does, and start
    /// chats in the background.
    #[arg(long)]
    pub resume: bool,
}

// Arguments of `shoal move ssh`.
#[derive(Debug, Args)]
pub struct SshArgs {
    /// SSH destination: a host, user@host or a ~/.ssh/config alias.
    pub host: String,
    #[arg(conflicts_with = "all", value_name = "SPACE")]
    pub space: Vec<String>,
    /// Move every workspace and chat of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose spaces --all moves.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// Leave the agents stopped on the other machine instead of queueing their restores; chats
    /// still start there, in the background.
    #[arg(long)]
    pub no_resume: bool,
    /// The shoal program on the other machine, when SSH's PATH does not find it.
    #[arg(long, value_name = "PATH", default_value = "shoal")]
    pub shoal: String,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}
