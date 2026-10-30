//! Arguments of `shoal move`.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::cli::args::ConfirmationArgs;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum MoveCommand {
    /// Remove a workspace and write it to a file that shoal move import opens on another machine.
    ///
    /// The file holds the removal's checkpoint, as a Git bundle without the commits of the
    /// default branch, the workspace's base, links and stopped agents, and the files of the
    /// agents' sessions, such as their transcripts. A removed workspace
    /// is exported without another removal. Without -o, or with `-`, it writes to standard
    /// output, which must not be a terminal. When -o names a directory, several workspaces, or
    /// with --all every workspace of the repository, go to a file each in it, once each one can
    /// be exported.
    Export(ExportArgs),
    /// Open workspaces shoal move export wrote, in the repository with the same origin.
    ///
    /// Fetches the default branch when a file builds on commits the repository lacks. It puts
    /// the agents' session files where the agents look for them, so shoal resume continues
    /// their sessions; one the file lacks continues only on the machine that exported it.
    /// --keep-sessions and --new-sessions override that. Without a file, or with `-`, it reads from standard
    /// input, which must not be a terminal. A directory imports each .shoal file in it.
    Import(ImportArgs),
    /// Move workspaces to another machine's Shoal over SSH, where their agents continue.
    ///
    /// Runs shoal move export | ssh HOST shoal move import --resume for each workspace, once
    /// ssh reaches Shoal there; nothing is removed when it does not. The other machine needs a
    /// repository with the same origin. A workspace whose import fails stays removed here, and
    /// shoal work restore opens it again.
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
    #[arg(conflicts_with = "all", value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// Export every workspace of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose workspaces --all exports.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// File to write, or a directory to write a file per workspace in, which several
    /// workspaces and --all need; defaults to standard output.
    #[arg(short, long, required_if_eq("all", "true"))]
    pub output: Option<PathBuf>,
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
    /// Continue every stopped agent's session, such as one whose files were copied by hand.
    #[arg(long, conflicts_with = "new_sessions")]
    pub keep_sessions: bool,
    /// Start new sessions even on the machine that exported the workspace.
    #[arg(long, conflicts_with = "keep_sessions")]
    pub new_sessions: bool,
    /// Queue the restores of the agents the export stopped, as shoal resume does.
    #[arg(long)]
    pub resume: bool,
}

// Arguments of `shoal move ssh`.
#[derive(Debug, Args)]
pub struct SshArgs {
    /// SSH destination: a host, user@host or a ~/.ssh/config alias.
    pub host: String,
    #[arg(conflicts_with = "all", value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// Move every workspace of the repository --repo names.
    #[arg(long, requires = "repository")]
    pub all: bool,
    /// Repository whose workspaces --all moves.
    #[arg(long = "repo", requires = "all")]
    pub repository: Option<String>,
    /// Leave the agents stopped on the other machine instead of queueing their restores.
    #[arg(long)]
    pub no_resume: bool,
    /// The shoal program on the other machine, when SSH's PATH does not find it.
    #[arg(long, value_name = "PATH", default_value = "shoal")]
    pub shoal: String,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}
