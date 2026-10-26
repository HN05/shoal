use std::ffi::OsString;

use clap::Args;

use crate::{agent::Agent, cli::args::AgentParser};

// Arguments of `shoal contribute`.
#[derive(Debug, Args)]
pub struct ContributeArgs {
    /// Run in the current pane instead of opening a Herdr tab.
    #[arg(long)]
    pub here: bool,
    /// Give the new tab or session the terminal, overriding the `focus` setting;
    /// its agent starts now, as with --now.
    #[arg(long, conflicts_with = "here")]
    pub focus: bool,
    /// Repository URL, or an issue, PR or branch URL.
    pub target: String,
    /// Git branch name; a portable workspace name is derived from it.
    pub branch: Option<String>,
    /// Start a built-in agent or a configured command after worktree creation.
    #[arg(long, value_parser = AgentParser)]
    pub agent: Option<Agent>,
    /// Start the agent now even when max_agents is reached, instead of queuing it.
    #[arg(long)]
    pub now: bool,
    /// Arguments forwarded to the agent.
    #[arg(last = true)]
    pub args: Vec<OsString>,
}
