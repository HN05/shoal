//! Arguments of commands that create workspaces.
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, item_reference},
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CreateCommand {
    /// Create or reopen a workspace.
    Add {
        /// Run in the current pane instead of opening a Herdr tab.
        #[arg(long)]
        here: bool,
        /// Give the new tab or session the terminal, overriding the `focus` setting;
        /// its agent starts now, as with --now.
        #[arg(long, conflicts_with = "here")]
        focus: bool,
        /// Registered repository, or an issue, PR or branch URL.
        #[arg(value_parser = item_reference)]
        repository: Option<String>,
        /// Repository for an issue number or a pasted link.
        #[arg(long = "repo")]
        repository_override: Option<String>,
        /// Git branch name; a portable workspace name is derived from it.
        branch: Option<String>,
        /// Use an existing local branch or remote/branch without creating a new branch.
        #[arg(long, conflicts_with_all = ["branch", "issue"])]
        existing: Option<String>,
        /// Derive a name and agent prompt from a forge issue number or URL.
        #[arg(long, value_parser = item_reference)]
        issue: Option<String>,
        /// Create this worktree at an exact new directory instead of the repository default.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its upstream;
        /// a remote branch such as origin/feature is fetched first);
        /// with --existing, the ref its changes are compared against.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Apply a named Git profile to the new worktree, overriding repository defaults.
        #[arg(long)]
        git_profile: Option<String>,
        /// Start a built-in agent or a configured command after worktree creation.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Start the agent now even when max_agents is reached, instead of queuing it.
        #[arg(long)]
        now: bool,
        /// Queue the agent until this workspace, queued agent or issue is complete;
        /// repeat for more. An issue's agent also waits for the issues blocking it.
        #[arg(long, value_name = "WORK", conflicts_with_all = ["now", "focus"])]
        after: Vec<String>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Create a workspace in someone else's repository for a PR.
    ///
    /// Shoal clones the repository and, when you cannot push to it, forks it
    /// and pushes your branches to the fork. Once nothing of it is left, it
    /// removes the clone.
    Contribute(ContributeArgs),
    /// Create a child workspace on a new branch for work in its own PR.
    ///
    /// The parent workspace's agent works on the child and receives its
    /// issue and PR activity; the parent's cleanup waits until its children
    /// are removed.
    #[command(group(clap::ArgGroup::new("child_agent").args(["agent", "fork"])))]
    Split {
        /// Git branch name; a portable workspace name is derived from it.
        #[arg(required_unless_present = "issue")]
        branch: Option<String>,
        /// Derive the branch name from a forge issue number or URL and link the issue.
        #[arg(long, value_parser = item_reference)]
        issue: Option<String>,
        /// Starting Git ref (defaults to the repository's default branch, refreshed from its
        /// upstream); the parent's branch stacks the child on it.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Parent workspace; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Start an agent of its own in the child, which reports to the parent's agent;
        /// it starts detached, from the agent queue.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Start the parent's agent in the child as --agent does, from a fork of its
        /// session, so it begins with the parent's conversation; the parent's session
        /// stays unchanged.
        #[arg(long)]
        fork: bool,
        /// Arguments forwarded to the agent, such as its prompt.
        #[arg(last = true, requires = "child_agent")]
        args: Vec<OsString>,
    },
}

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
