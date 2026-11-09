//! Arguments of `shoal config`, `shoal update` and `shoal daemon`.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, ModelArgs},
};

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
        /// Edit this repository's local config instead of the global file.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
    },
    /// Remove config keys or tables, falling back to lower layers or defaults.
    Unset {
        #[arg(required = true)]
        keys: Vec<String>,
        /// Edit this repository's local config instead of the global file.
        #[arg(short = 'r', long = "repo")]
        repository: Option<String>,
    },
    /// Show effective repository settings and the layer each value came from.
    Show {
        workspace: Option<SpaceSelector>,
        /// Show this repository's settings in its registered checkout instead of a workspace's.
        #[arg(short = 'r', long = "repo", conflicts_with = "workspace")]
        repository: Option<String>,
    },
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
    /// Have an agent write the global config in a chat without a repository,
    /// installing the default config first when there is none.
    Setup(Box<ConfigSetupArgs>),
}

// Arguments of `shoal config setup`.
#[derive(Debug, Args)]
pub struct ConfigSetupArgs {
    /// Run in this terminal instead of a new tab or detached session.
    #[arg(short = 'H', long)]
    pub here: bool,
    /// Agent to start; defaults to agent.default, otherwise a picker.
    #[arg(short = 'a', long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    #[command(flatten)]
    pub model: ModelArgs,
    /// Name of the chat; defaults to setup. A chat that reopens in a new tab
    /// or a restore passes it.
    #[arg(long, hide = true)]
    pub name: Option<crate::client::space_names::SpaceName>,
    /// Agent session the chat continues, which shoal resume passes.
    #[arg(long, hide = true)]
    pub session: Option<crate::client::agent::AgentSessionId>,
    /// Stopped chat, by ID, that this start restores under its own name,
    /// which shoal resume passes.
    #[arg(long, hide = true, requires = "name")]
    pub restore: Option<SpaceId>,
    /// Handoff of the starter that waits on this background place.
    #[arg(long, hide = true, requires = "here")]
    pub handoff: Option<PathBuf>,
    /// Arguments forwarded to the agent.
    #[arg(last = true)]
    pub args: Vec<OsString>,
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
