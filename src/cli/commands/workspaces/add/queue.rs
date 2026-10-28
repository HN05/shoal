//! Admitting an addition's agent to the agent queue, with the work it waits for.
use anyhow::Result;

use super::{
    AddOptions,
    agent::AddSettings,
    plan::{AddPlan, planned_workspace_name},
    target::AddTarget,
};
use crate::{
    cli::{
        client::request,
        commands::{agent_queue, issues},
        context::Context,
    },
    forge::IssueInput,
    model::Dependency,
    protocol::WorkspaceMethod,
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
    request::<()>(
        &ctx.paths,
        WorkspaceMethod::CheckDependencies {
            repository: target.selector.clone(),
            after: dependencies.clone(),
        },
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
    let now = options.starts_now(plan, settings.get().await?.focus, ctx.interactive());
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
    let repo = target.repository(ctx).await?;
    match issues::open_dependencies(ctx, repo, issue).await {
        Ok(urls) => Ok(urls.into_iter().map(Dependency::Issue).collect()),
        Err(error) => {
            eprintln!(
                "warning: cannot list the issues blocking #{}, so the agent does not wait for them: {error:#}",
                issue.number
            );
            Ok(Vec::new())
        }
    }
}
