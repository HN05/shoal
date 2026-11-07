//! Admitting an addition's agent to the agent queue, with the work it waits for.
use anyhow::Result;

use super::{AddOptions, agent::AddSettings, plan::planned_workspace_name, target::AddTarget};
use crate::{
    cli::{
        client::{self},
        commands::{agent_queue, issues},
        context::Context,
    },
    forge::IssueInput,
    model::Dependency,
    start_plan::AddPlan,
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
        dependencies.push(match names_issue(input) {
            true => Dependency::Issue(issues::url(ctx, repo, input).await?),
            false => Dependency::Work(input.clone()),
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

/// Whether an `--after` value names an issue, by number or URL, as `shoal
/// start` takes one: unlike every other space argument, a bare number here
/// is an issue, never a space's number.
pub(super) fn names_issue(input: &str) -> bool {
    IssueInput::parse(input) != IssueInput::Invalid
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
    if plan.agent_name().is_none() {
        return Ok(None);
    }
    let now = options.starts_now(
        plan,
        settings.get().await?.terminal.focus,
        ctx.interactive(),
    );
    let after = launch_dependencies(ctx, target, issue, now, after).await?;
    admit_launch(ctx, target, plan, issue, now, after).await
}

/// [`admit`] the plan's agent, which starts `now` or waits for `after`.
pub(super) async fn admit_launch(
    ctx: &Context,
    target: &AddTarget,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    now: bool,
    after: Vec<Dependency>,
) -> Result<Option<i32>> {
    let Some(agent) = plan.agent_name() else {
        return Ok(None);
    };
    let launch = agent_queue::Launch {
        agent,
        name: planned_workspace_name(ctx, target, &plan.creation).await?,
        issue_title: issue.map(|issue| issue.title.clone()),
        now,
        after,
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
pub(super) async fn launch_dependencies(
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
