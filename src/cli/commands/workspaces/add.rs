//! `shoal add`: plan a workspace, then open it here or hand it to a terminal host.
mod agent;
mod finish;
mod handoff;
mod plan;
mod queue;
mod source;
mod target;

pub(in crate::cli::commands) use agent::AgentLaunch;
pub(in crate::cli) use finish::execute_add;
pub(in crate::cli) use handoff::start_queued;
pub(crate) use plan::queued_agent_request;
pub(in crate::cli) use plan::{AddPlan, Creation};
use std::ffi::OsString;

use anyhow::{Result, ensure};

use self::{
    agent::{AddSettings, choose_add_agent, choose_for_picked_issue, resolve_add_agent},
    finish::execute_add_with_target,
    handoff::start_beside,
    plan::{check_add_creation, plan_addition},
    queue::{admit, after_dependencies, failing_entry},
    source::{load_add_issue, pick_add_source, select_add_creation},
    target::resolve_add_target,
};
use crate::cli::context::Context;

/// How `shoal add` runs once its workspace is planned.
#[derive(Debug, Clone, Default)]
pub(in crate::cli::commands) struct AddOptions {
    /// Run in the current pane instead of opening a Herdr tab.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Start the agent even when `max_agents` is reached.
    pub now: bool,
    /// Workspaces, queued agents and issues the agent waits for.
    pub after: Vec<String>,
}

impl AddOptions {
    /// Whether `plan`'s agent skips the queue: the user waits for a focused
    /// launch, so it starts at once as `--now` does. That is a launch `--focus`
    /// focuses, or, from an `interactive` call, one waiting for the user's
    /// input that the `focus` setting does not send to the background; the
    /// setting alone does not, so it cannot switch the limit off. A launch
    /// that waits for work queues.
    fn starts_now(&self, plan: &AddPlan, focus_setting: Option<bool>, interactive: bool) -> bool {
        let waits_for_user = interactive && !plan.runs_unattended();
        self.now
            || self.focus
            || (self.after.is_empty() && waits_for_user && focus_setting != Some(false))
    }
}

pub(in crate::cli::commands) async fn add(
    ctx: &Context,
    repository: Option<String>,
    creation: Creation,
    issue: Option<String>,
    agent: AgentLaunch,
    args: Vec<OsString>,
    options: AddOptions,
) -> Result<i32> {
    let mut creation = creation.absolute(ctx)?;
    let target = resolve_add_target(ctx, repository, issue.as_deref()).await?;
    check_add_creation(ctx, &target, &creation).await?;
    let after = after_dependencies(ctx, &target, &options.after).await?;
    let settings = AddSettings::new(ctx, &target.selector);
    let agent = choose_add_agent(&settings, agent).await?;
    let picked = match issue {
        Some(_) => None,
        None => pick_add_source(ctx, &target, &mut creation).await?,
    };
    let agent = choose_for_picked_issue(&settings, agent, picked.is_some()).await?;
    let issue = load_add_issue(ctx, &target, issue.or(picked)).await?;
    let creation = select_add_creation(ctx, &target, creation, issue.as_ref()).await?;
    // An issue or a typed name settles the branch; check it before the agent picker.
    check_add_creation(ctx, &target, &creation).await?;
    let agent = resolve_add_agent(ctx, &settings, agent).await?;
    let mut plan = plan_addition(ctx, &target, creation, issue.as_ref(), agent, args).await?;
    ensure!(
        options.after.is_empty() || plan.agent_name().is_some(),
        "--after queues an agent; name one with --agent"
    );
    if let Some(queued) = admit(
        ctx,
        &target,
        &settings,
        &mut plan,
        issue.as_ref(),
        &options,
        after,
    )
    .await?
    {
        return Ok(queued);
    }
    let handoff = start_beside(ctx, &target, &settings, &mut plan, issue.as_ref(), options).await;
    if failing_entry(ctx, plan.queue_entry.as_deref(), handoff).await? {
        return Ok(0);
    }
    execute_add_with_target(ctx, plan, target, issue).await
}

#[cfg(test)]
mod tests;
