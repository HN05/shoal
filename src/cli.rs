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
    /// Enter a workspace through the shell integration (otherwise print its path).
    Cd { workspace: Option<String> },
    /// Show your changes since the fork point using native Git diff configuration.
    Diff { workspace: Option<String> },
    /// Reserve, list, and release named TCP ports owned by a worktree.
    Port {
        #[command(subcommand)]
        command: PortCommand,
    },
    /// Inspect a workspace and its executions.
    Inspect { workspace: Option<String> },
    /// Stop connected commands, preserving the workspace.
    Stop { workspace: Option<String> },
    /// Remove a worktree and its redundant branch; choose what to keep if work differs.
    Rm {
        workspace: Option<String>,
        /// Confirm removal without a prompt; differing/dirty work needs a branch choice.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Remove the worktree but retain its branch, including when it contains work.
        #[arg(long, requires = "yes", conflicts_with = "delete_branch")]
        keep_branch: bool,
        /// Remove both worktree and branch, including uncommitted and differing work.
        #[arg(long, requires = "yes", conflicts_with = "keep_branch")]
        delete_branch: bool,
    },
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
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
    },
    Rename {
        repository: String,
        name: String,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum PortCommand {
    /// Reserve a port, or return the existing reservation with this name.
    Reserve {
        name: String,
        workspace: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Environment variable exported to subsequent exec/claude/codex commands.
        #[arg(long)]
        env: Option<String>,
        /// Explain what the reservation is used for.
        #[arg(long)]
        reason: Option<String>,
    },
    List {
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    Release {
        name: String,
        workspace: Option<String>,
    },
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
