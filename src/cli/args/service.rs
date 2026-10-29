//! Arguments of `shoal config`, `shoal update` and `shoal daemon`.
use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ServiceCommand {
    /// Show and change Shoal configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Replace a downloaded shoal with the latest GitHub release.
    Update,
    /// Inspect and control the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ConfigCommand {
    /// Set config values using TOML dotted keys (global by default), saved together.
    Set {
        /// KEY VALUE pairs; each value is TOML, or an unquoted string.
        #[arg(required = true, num_args = 2.., value_names = ["KEY", "VALUE"])]
        assignments: Vec<String>,
        /// Edit this repository's saved config instead of the global file.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Remove config keys or tables, falling back to lower layers or defaults.
    Unset {
        #[arg(required = true)]
        keys: Vec<String>,
        /// Edit this repository's saved config instead of the global file.
        #[arg(long = "repo")]
        repository: Option<String>,
    },
    /// Show effective repository settings and the layer each value came from.
    Show { workspace: Option<String> },
    /// Print the command and configuration reference for this release.
    Reference,
    /// Install a packaged global config, keeping the old file as config.toml.backup.
    Install {
        /// Packaged config; omit to choose one.
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(
            crate::config::PACKAGED.iter().map(|(name, _)| *name)
        ))]
        name: Option<String>,
    },
    /// Rewrite the config with the defaults; the old file becomes config.toml.backup.
    Reset,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum DaemonCommand {
    /// Install and start the per-user daemon service.
    Install {
        /// Preview the service definition without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Executable path to register (preserves symlinks for package upgrades).
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    Status,
    Start,
    Stop,
    Restart,
    /// Reread the global config without stopping running agents.
    Reload,
    /// Run in the foreground, without registering an OS service.
    Run {
        #[arg(long, hide = true)]
        managed: bool,
    },
}
