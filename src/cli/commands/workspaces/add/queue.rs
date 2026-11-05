//! Admitting an addition's agent to the agent queue, with the work it waits for.
use anyhow::{Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    plan::{AddPlan, planned_workspace_name},
    target::AddTarget,
};
use crate::{
    cli::{
        client::{self},
        commands::{agent_queue, issues},
        context::Context,
    },
    forge::IssueInput,
    model::Dependency,
};

/// The work `--after` names, refused before any prompt when the repository
/// has no such workspace, queued agent or issue.
pub(super) async fn after_dependencies(
    ctx: &Context,
    target: &AddTarget,
    after: &[String],
) -> Result<Vec<Dependency>> {
    if after.is_empty() {
        return Ok(Vec::new());
    }
    let repo = target.repository(ctx).await?;
    let mut dependencies = Vec::new();
    for input in after {
        dependencies.push(match IssueInput::parse(input) {
            IssueInput::Number | IssueInput::Url => {
                Dependency::Issue(issues::url(ctx, repo, input).await?)
            }
            IssueInput::Invalid => Dependency::Work(input.clone()),
        });
    }
    crate::client::workspaces::check_dependencies(
        &ctx.paths,
        target.selector.clone(),
        dependencies.clone(),
    )
    .await?;
    Ok(dependencies)
}

/// Admit the plan's agent, if any, to the agent queue: a start takes its
/// slot and builds on the branch a done dependency left, while a queued
/// launch is reported and returns its exit status.
pub(super) async fn admit(
    ctx: &Context,
    target: &AddTarget,
    settings: &AddSettings<'_>,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    options: &AddOptions,
    after: Vec<Dependency>,
) -> Result<Option<i32>> {
    let Some(agent) = plan.agent_name() else {
        return Ok(None);
    };
    let now = options.starts_now(
        plan,
        settings.get().await?.terminal.focus,
        ctx.interactive(),
    );
    let launch = agent_queue::Launch {
        agent,
        name: planned_workspace_name(ctx, target, &plan.creation).await?,
        issue_title: issue.map(|issue| issue.title.clone()),
        now,
        after: launch_dependencies(ctx, target, issue, now, after).await?,
    };
    match agent_queue::admit(ctx, plan, launch).await? {
        agent_queue::Admission::Start { entry, base } => {
            plan.queue_entry = entry;
            plan.creation.build_on(base);
            Ok(None)
        }
        agent_queue::Admission::Queued(entry, waiting) => {
            agent_queue::report_queued(ctx, &entry, waiting).map(Some)
        }
    }
}

/// Refuse a scoped caller's agent that is not queued as soon as its new
/// workspace exists: one that starts at once in this terminal, as `--now`
/// and `--focus` ask, or one that waits for `after` or its issue's blockers,
/// whose branch a workspace created later would build on.
pub(super) async fn ensure_queues_at_once(
    ctx: &Context,
    target: &AddTarget,
    issue: Option<&issues::Issue>,
    options: &AddOptions,
    after: Vec<Dependency>,
) -> Result<()> {
    ensure!(
        !options.now && !options.focus,
        "scoped processes start agents when a slot is free; leave out --now and --focus"
    );
    let after = launch_dependencies(ctx, target, issue, false, after).await?;
    let waits_for: Vec<_> = (after.iter())
        .map(|dependency| match dependency {
            Dependency::Work(name) | Dependency::Issue(name) => name.as_str(),
        })
        .collect();
    ensure!(
        waits_for.is_empty(),
        "scoped processes add a workspace at once, so its agent cannot wait for other work; add it once {} is complete",
        waits_for.join(", ")
    );
    Ok(())
}

/// `result`, after failing the queue `entry`, if any, with its error.
pub(super) async fn failing_entry<T>(
    ctx: &Context,
    entry: Option<&str>,
    result: Result<T>,
) -> Result<T> {
    if let (Some(entry), Err(error)) = (entry, &result) {
        agent_queue::fail(ctx, entry, error).await;
    }
    result
}

/// The work a launch waits for: `after`, and the open issues the forge
/// records as blocking its issue. A launch that starts `now` waits for nothing.
async fn launch_dependencies(
    ctx: &Context,
    target: &AddTarget,
    issue: Option<&issues::Issue>,
    now: bool,
    mut after: Vec<Dependency>,
) -> Result<Vec<Dependency>> {
    if now {
        return Ok(Vec::new());
    }
    if let Some(issue) = issue {
        after.extend(issue_blockers(ctx, target, issue).await?);
    }
    Ok(after)
}

/// The open issues the forge records as blocking `issue`. A forge that
/// cannot list them, such as a repository with dependencies turned off,
/// leaves a warning and none.
async fn issue_blockers(
    ctx: &Context,
    target: &AddTarget,
    issue: &issues::Issue,
) -> Result<Vec<Dependency>> {
    let context = issues::read_context(ctx, target.repository(ctx).await?).await?;
    match client::issue_blockers(&ctx.paths, context, issue.url.clone()).await {
        Ok(blockers) => Ok(blockers),
        Err(error) => {
            eprintln!(
                "warning: cannot list the issues blocking #{}, so the agent does not wait for them: {error:#}",
                issue.number
            );
            Ok(Vec::new())
        }
    }
}
