//! Arguments of commands that open agents and apps in a workspace.
use std::ffi::OsString;

use clap::Subcommand;

use crate::cli::args::ModelArgs;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AgentsCommand {
    /// Run Claude Code or open the Claude app.
    Claude {
        workspace: Option<String>,
        /// Run the Claude Code CLI, overriding agents.claude.default_mode.
        #[arg(long, conflicts_with = "app")]
        cli: bool,
        /// Open the workspace in the Claude app, overriding agents.claude.default_mode.
        #[arg(long, conflicts_with_all = ["cli", "model", "effort"])]
        app: bool,
        #[command(flatten)]
        model: ModelArgs,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Run Codex CLI or open the Codex app.
    Codex {
        workspace: Option<String>,
        /// Run the Codex CLI, overriding agents.codex.default_mode.
        #[arg(long, conflicts_with = "app")]
        cli: bool,
        /// Open the workspace in the Codex app, overriding agents.codex.default_mode.
        #[arg(long, conflicts_with_all = ["cli", "model", "effort"])]
        app: bool,
        #[command(flatten)]
        model: ModelArgs,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Open a workspace in the running T3 Code app.
    T3 {
        workspace: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Start a detached agent session in Happy.
    #[cfg(feature = "happy")]
    Happy {
        #[arg(value_enum)]
        agent: crate::happy::Agent,
        workspace: Option<String>,
        /// First message for the session; delivered through Happy's server when the agent takes no prompt argument.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}
