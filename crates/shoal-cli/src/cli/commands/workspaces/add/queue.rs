//! Admitting an addition's agent to the agent queue, with what blocks it.
use anyhow::Result;

use crate::client::queue::QueueEntryId;

use super::{
    AddOptions,
    agent::AddSettings,
    plan::{planned_workspace_name, reopened_workspace},
    target::AddTarget,
};
use crate::{
    cli::{
        client::{self},
        commands::{self, issues, queued_agents},
        context::Context,
    },
    model::Blocker,
    start_plan::AddPlan,
};

/// What `--add-blocker` names, refused before any prompt when the repository
/// has no such workspace, queued agent or issue.
pub(super) async fn named_blockers(
    ctx: &Context,
    target: &AddTarget,
    named: &[String],
) -> Result<Vec<Blocker>> {
    if named.is_empty() {
        return Ok(Vec::new());
    }
    let repo = target.repository(ctx).await?;
    let mut blockers = Vec::new();
    for input in named {
        blockers.push(match commands::blockers::parse(input)? {
            Blocker::Issue(issue) => Blocker::Issue(issues::issue(ctx, repo, &issue).await?.into()),
            blocker => blocker,
        });
    }
    crate::client::workspaces::check_blockers(
        &ctx.paths,
        target.selector.clone(),
        blockers.clone(),
    )
    .await?;
    Ok(blockers)
}

/// Admit the plan's agent, if any, to the agent queue: a start takes its
/// slot and builds on the branch a done blocker left, while a queued
/// launch is reported and returns its exit status.
pub(super) async fn admit(
    ctx: &Context,
    target: &AddTarget,
    settings: &AddSettings<'_>,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    options: &AddOptions,
    blockers: Vec<Blocker>,
) -> Result<Option<i32>> {
    if plan.agent_name().is_none() {
        return Ok(None);
    }
    let now = options.starts_now(
        plan,
        settings.get().await?.focus(&plan.mode()),
        ctx.on_terminal(),
    );
    let blockers = launch_blockers(ctx, target, issue, now, blockers).await?;
    admit_launch(ctx, target, plan, issue, now, blockers).await
}

/// [`admit`] the plan's agent, which starts `now` or waits for `blockers`.
pub(super) async fn admit_launch(
    ctx: &Context,
    target: &AddTarget,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    now: bool,
    blockers: Vec<Blocker>,
) -> Result<Option<i32>> {
    let Some(agent) = plan.agent_name() else {
        return Ok(None);
    };
    // The plan names its repository by ID, as the workspace it may reopen
    // does, however the start named it.
    plan.repository = (&target.repository(ctx).await?.id).into();
    let launch = queued_agents::Launch {
        agent,
        name: planned_workspace_name(ctx, target, &plan.creation).await?,
        issue_title: issue.map(|issue| issue.title.clone()),
        issue_labels: issue.map_or_else(Vec::new, |issue| issue.labels.clone()),
        now,
        blockers,
        opens: reopened_workspace(ctx, target, plan).await?,
        // The daemon starts a scoped caller's agents.
        caller_starts: !crate::env::is_scoped(),
    };
    match queued_agents::admit(ctx, plan, launch).await? {
        queued_agents::Admission::Start { entry, base } => {
            plan.queue_entry = entry;
            plan.creation.build_on(base);
            Ok(None)
        }
        queued_agents::Admission::Queued(entry, waiting) => {
            queued_agents::report_queued(ctx, &entry, waiting).map(Some)
        }
    }
}

/// `result`, after failing the queue `entry`, if any, with its error.
pub(super) async fn failing_entry<T>(
    ctx: &Context,
    entry: Option<&QueueEntryId>,
    result: Result<T>,
) -> Result<T> {
    if let (Some(entry), Err(error)) = (entry, &result) {
        queued_agents::fail(ctx, entry, error).await;
    }
    result
}

/// What blocks a launch: `blockers`, and the open issues the forge records
/// as blocking its issue. A launch that starts `now` waits for nothing.
pub(super) async fn launch_blockers(
    ctx: &Context,
    target: &AddTarget,
    issue: Option<&issues::Issue>,
    now: bool,
    mut blockers: Vec<Blocker>,
) -> Result<Vec<Blocker>> {
    if now {
        return Ok(Vec::new());
    }
    if let Some(issue) = issue {
        blockers.extend(issue_blockers(ctx, target, issue).await?);
    }
    Ok(blockers)
}

/// The open issues the forge records as blocking `issue`. A forge that
/// cannot list them, such as a repository with blocking issues turned off,
/// leaves a warning and none.
async fn issue_blockers(
    ctx: &Context,
    target: &AddTarget,
    issue: &issues::Issue,
) -> Result<Vec<Blocker>> {
    let context = issues::read_context(ctx, target.repository(ctx).await?).await?;
    match client::issue_blockers(&ctx.paths, context, issue.url.clone().into()).await {
        Ok(blockers) => Ok(blockers),
        Err(error) => {
            eprintln!(
                "warning: cannot list the issues blocking #{}, so they do not block the agent: {error:#}",
                issue.number
            );
            Ok(Vec::new())
        }
    }
}
