use std::{ffi::OsString, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Override Shoal's state directory (also isolates the daemon).
    #[arg(long, global = true, env = "SHOAL_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// Emit machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print shell integration for Bash or Zsh.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    /// Register repositories and list recently used repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Create a named worktree from a registered repository.
    Add {
        repository: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Starting Git ref (defaults to the registered checkout's HEAD).
        #[arg(long = "ref")]
        base: Option<String>,
    },
    /// List managed workspaces.
    List,
    /// Inspect a workspace and its executions.
    Inspect { workspace: Option<String> },
    /// Stop connected commands, preserving the workspace.
    Stop { workspace: Option<String> },
    /// Stop commands and remove a clean worktree, preserving its branch.
    Rm { workspace: Option<String> },
    /// Execute a command in a named or current workspace.
    Exec {
        workspace: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Shortcut for exec -- claude.
    Claude {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Shortcut for exec -- codex.
    Codex {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Install and start the per-user daemon service.
    Setup {
        /// Preview the service definition without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Executable path to register (preserves symlinks for package upgrades).
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    /// Inspect and control the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    Add { source: String },
    List,
}

#[derive(Debug, Subcommand)]
pub enum ShellCommand {
    Init,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    Status,
    Start,
    Stop,
    Restart,
    /// Run in the foreground, without registering an OS service.
    Run {
        #[arg(long, hide = true)]
        managed: bool,
    },
}
