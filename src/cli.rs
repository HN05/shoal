use std::path::PathBuf;

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
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
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
