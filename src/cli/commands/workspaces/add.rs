//! `shoal start`: plan a workspace, then open it here or hand it to a terminal host.
mod agent;
mod detached;
mod finish;
mod handoff;
mod place;
mod plan;
mod queue;
mod source;
mod target;

pub(in crate::cli::commands) use agent::AgentLaunch;
pub(in crate::cli) use finish::execute_add;
pub(in crate::cli) use handoff::start_queued;
pub(in crate::cli) use plan::{AddPlan, CopyCreation, Creation};
pub(crate) use plan::{check_queued_launch, queued_agent_request};
use std::ffi::OsString;

use anyhow::{Result, ensure};

use self::{
    agent::{
        AddSettings, check_chosen_model, choose_add_agent, choose_for_picked_issue,
        resolve_add_agent,
    },
    detached::{add_detached, ensure_detachable, starts_detached},
    finish::execute_add_with_target,
    handoff::start_beside,
    plan::{check_add_creation, plan_addition},
    queue::{admit, after_dependencies, failing_entry},
    source::{load_add_issue, pick_add_source, select_add_creation},
    target::resolve_add_target,
};
use crate::cli::context::Context;

/// How `shoal start` runs once its workspace is planned.
#[derive(Debug, Clone, Default)]
pub(in crate::cli::commands) struct AddOptions {
    /// Run in this terminal instead of a new tab or detached session.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Start the agent even when `max_agents` is reached.
    pub now: bool,
    /// Workspaces, queued agents and issues the agent waits for.
    pub after: Vec<String>,
    /// The model and effort requested over the configured ones.
    pub model: crate::agent::ModelChoice,
    /// The mode the agent starts in; the default mode when `None`.
    pub mode: Option<String>,
    /// The space the workspace goes below, instead of the chat this runs in
    /// or, for a workspace process, its own space.
    pub parent: Option<String>,
    /// The session the agent starts from a fork of.
    pub fork: Option<String>,
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
    // Without its token, the daemon would add a workspace process's
    // workspace for the user instead of below its own.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    let mut creation = creation.absolute(ctx)?;
    let target = resolve_add_target(ctx, repository, issue.as_deref()).await?;
    check_add_creation(ctx, &target, &creation, options.parent.as_deref()).await?;
    let after = after_dependencies(ctx, &target, &options.after).await?;
    let settings = AddSettings::new(ctx, &target.selector);
    let agent = choose_add_agent(&settings, agent).await?;
    check_chosen_model(&settings, &agent, &options.model).await?;
    let picked = match issue {
        Some(_) => None,
        None => pick_add_source(ctx, &target, &mut creation).await?,
    };
    let agent = choose_for_picked_issue(&settings, agent, picked.is_some()).await?;
    let agent = agent::for_creation(&creation, agent)?;
    let issue = load_add_issue(ctx, &target, issue.or(picked)).await?;
    let creation = select_add_creation(ctx, &target, creation, issue.as_ref()).await?;
    // An issue or a typed name settles the branch; check it before the agent picker.
    check_add_creation(ctx, &target, &creation, options.parent.as_deref()).await?;
    let agent = resolve_add_agent(ctx, &settings, agent, options.model.clone()).await?;
    let mut plan = plan_addition(ctx, &target, creation, issue.as_ref(), agent, args).await?;
    plan.start_as(&options);
    ensure!(
        options.after.is_empty() || plan.agent_name().is_some(),
        "--after queues an agent; name one with --agent"
    );
    if starts_detached(ctx, &plan, &options)? {
        ensure_detachable(&settings, &plan, &options).await?;
        return add_detached(ctx, plan, target, issue, options, after).await;
    }
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
