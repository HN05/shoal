//! `shoal start` of a workspace: what a terminal picks, the one launch the
//! daemon carries out, and the agent that runs in this caller.
mod agent;
mod attach;
mod detached;
mod existing;
mod finish;
mod launch;
mod mode;
mod plan;
mod reads;
mod resolve;
mod source;
mod target;
mod waiting;

pub(in crate::cli::commands) use detached::leaves_caller;
pub(in crate::cli::commands) use existing::start_worker;
pub(in crate::cli) use finish::execute_add;
pub(in crate::cli::commands) use launch::{launch_workspace, open_here, pick_start_source};
pub(in crate::cli::commands) use plan::launch_args;
pub(in crate::cli::commands) use reads::DaemonReads;
pub(in crate::cli::commands) use resolve::check_start_arguments;
use shoal_protocol::space_names::SpaceSelector;

use anyhow::Result;

use self::{plan::check_add_creation, target::resolve_add_target};
use crate::{
    cli::{context::Context, ui::ModeChoice},
    forge::items::reference::ItemInput,
    start_plan::Creation,
};

/// How `shoal start` runs its agent once its workspace is picked.
#[derive(Debug, Clone, Default)]
pub(in crate::cli::commands) struct AddOptions {
    /// Run in this terminal instead of a new tab or detached session.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Wait for a new workspace's preparation before returning.
    pub wait: bool,
    /// Start the agent even when `max_heavy_agents` is reached.
    pub now: bool,
    /// Workspaces, queued agents and issues that block the agent.
    pub blockers: Vec<String>,
    /// The model and effort requested over the configured ones.
    pub model: crate::agent::ModelChoice,
    /// The mode the agent starts in; the default mode when `None`.
    pub mode: Option<ModeChoice>,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The session the agent starts from a fork of.
    pub fork: Option<shoal_ids::agent_session_ids::AgentSessionId>,
}

/// A workspace's start that names no branch, issue, PR, existing branch or
/// copy, which begins as a chat in `repository` in `mode` instead, whose
/// agent makes it a workspace once it knows the work.
#[derive(Debug)]
pub(in crate::cli::commands) struct StartsAsChat {
    pub repository: crate::repository::Selector,
    pub mode: Option<crate::config::modes::ModeName>,
}

impl std::fmt::Display for StartsAsChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("name the workspace's branch, or pass --issue")
    }
}

impl std::error::Error for StartsAsChat {}

/// The repository a start goes in, the one `repository` names or `issue`
/// belongs to, else the current or a picked one, once the daemon accepts a
/// workspace there below `parent`. A start checks it before asking what it
/// starts, so its refusals come first.
pub(in crate::cli::commands) async fn check_target(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
    issue: Option<&ItemInput>,
    parent: Option<&SpaceSelector>,
) -> Result<crate::repository::Selector> {
    let target = resolve_add_target(ctx, repository, issue).await?;
    check_add_creation(ctx, &target, &Creation::default(), parent).await?;
    Ok(target.selector)
}
