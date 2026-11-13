//! Queuing the restores of stopped agents, which the daemon starts as the
//! agent limits allow, or at once with `--now`.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Context as _, Result, ensure};

use super::super::targets::Selected;
use super::{commands, permission_change, reports, stale};
use crate::client::output::resume::Queued;
use crate::client::saved::{Queuing, Restore};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::{PresentSpace, QueueEntry},
    protocol::ConfigTarget,
    terminal,
};

/// Queue the restores of the stopped agents and last sessions of the
/// `selected` workspaces below this process's, only `execution`'s when
/// given, restore the `selected` chats in a background place and retry the
/// selected failed setups, under `change` when given; the daemon refuses
/// those this process or one below it did not stop, before any setup or
/// restore, and starts the workspaces' agents detached.
pub(super) async fn queue_below(
    ctx: &Context,
    selected: Selected,
    execution: Option<ExecutionId>,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let mut stopped = Vec::new();
    for selector in selected.spaces.clone().unwrap_or_default() {
        stopped.push(stopped_agents(ctx, selector, execution.as_ref()).await?);
    }
    permission_change::apply(ctx, &stopped, &selected.setups, change).await?;
    let resumed = super::set_up(ctx, &selected, Vec::new(), false).await;
    queue_all(ctx, stopped, resumed, false).await
}

/// Executions stopped so their workspaces could move, by workspace.
pub(in crate::cli::commands) type Stopped =
    std::collections::BTreeMap<SpaceId, std::collections::BTreeSet<ExecutionId>>;

/// Queue the restores of the agents among `stopped`; their workspaces'
/// stopped commands go to them, and those of workspaces without one stay
/// for `shoal restore`.
pub(in crate::cli::commands) async fn queue_stopped(
    ctx: &Context,
    stopped: &Stopped,
) -> Result<Queued> {
    let mut queued = Vec::new();
    for (space, executions) in stopped {
        let space = super::present_space(ctx, &SpaceSelector::id(space)).await?;
        let mut saved = saved(ctx, &space).await?;
        saved.retain(|id| executions.contains(id));
        if !(saved.agents.is_empty() && saved.sessions.is_empty()) {
            queued.push((space, saved));
        }
    }
    Ok(queue_each(ctx, queued, false).await)
}

/// The stopped agents and last sessions of the workspace `selector` names,
/// only `execution`'s when given, refusing one without any. Its stopped
/// commands go to the restored agents.
async fn stopped_agents(
    ctx: &Context,
    selector: SpaceSelector,
    execution: Option<&ExecutionId>,
) -> Result<(PresentSpace, Saved)> {
    let space = super::present_space(ctx, &selector).await?;
    let mut saved = saved(ctx, &space).await?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, true);
    }
    saved.retain_unattended();
    saved.commands.clear();
    ensure!(
        !(saved.agents.is_empty() && saved.sessions.is_empty()),
        "{} has no stopped agent to restore",
        space.name()
    );
    Ok((space, saved))
}

/// Restore `workspace`'s stopped agents, only `execution`'s when given, as
/// [`Saved::restore`] decides for `queuing`, making `change` when given.
pub(super) async fn queue_space(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let space = super::present_space(ctx, &space).await?;
    let mut saved = saved(ctx, &space).await?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this {}",
        space.kind()
    );
    let now = match saved.restore(queuing) {
        Restore::Continue => {
            return super::space::continue_last_session(ctx, &space, saved, false, change).await;
        }
        Restore::Queue { now } => now,
    };
    let stopped = vec![(space, saved)];
    if matches!(queuing, Queuing::Daemon { .. }) {
        ensure_daemon_starts(ctx, &stopped).await?;
    }
    permission_change::apply(ctx, &stopped, &[], change).await?;
    queue_all(ctx, stopped, Queued::default(), now).await
}

/// What `workspace` saved.
async fn saved(ctx: &Context, space: &PresentSpace) -> Result<Saved> {
    crate::client::saved::read(&ctx.paths, SpaceSelector::id(space.id())).await
}

/// Refuse, before any is queued, the restores of `stopped` agents that the
/// daemon would start from the background when no host can take them there;
/// a waiting wrapper restores its agent where it stopped.
pub(super) async fn ensure_daemon_starts(
    ctx: &Context,
    stopped: &[(PresentSpace, Saved)],
) -> Result<()> {
    for (space, saved) in stopped {
        if !saved.restores_outside_wrappers() {
            continue;
        }
        let target = ConfigTarget::Space(SpaceSelector::id(space.id()));
        let settings = crate::config::settings(&ctx.paths, target).await?;
        crate::cli::terminal::ensure_background(&settings).with_context(|| {
            format!(
                "cannot restore the agents of {} in the background; pass --focus to restore one here",
                space.name()
            )
        })?;
    }
    Ok(())
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once regardless of the limits when `now`, and report
/// the commands of workspaces without one, going on after one fails, with
/// the stopped chats, setups and failures `resumed` reports already.
pub(super) async fn queue_all(
    ctx: &Context,
    stopped: Vec<(PresentSpace, Saved)>,
    resumed: Queued,
    now: bool,
) -> Result<i32> {
    let mut queued = queue_each(ctx, stopped, now).await;
    let mut failed = resumed.failed;
    failed.append(&mut queued.failed);
    queued.failed = failed;
    queued.set_up = resumed.set_up;
    queued.reopened = resumed.reopened;
    reports::queued(ctx, &queued)?;
    Ok(if queued.failed.is_empty() { 0 } else { 1 })
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once when `now`, and report the commands of workspaces
/// without one that restores outside a waiting wrapper, going on after one
/// fails, which it reports.
pub(super) async fn queue_each(
    ctx: &Context,
    stopped: Vec<(PresentSpace, Saved)>,
    now: bool,
) -> Queued {
    let mut queued = Vec::new();
    let mut commands = Vec::new();
    let mut failed = Vec::new();
    for (space, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            let _claims = commands::claim(&mut saved.commands);
            match commands::report(ctx, &space, &saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(ui::failure(ctx, "restore", space.name(), &error)),
            }
        }
        for saved in saved.agents.iter().chain(&saved.sessions) {
            match queue(ctx, &space, &saved.id, now).await {
                Ok(entry) => queued.push(entry),
                Err(error) => failed.push(ui::failure(ctx, "restore", space.name(), &error)),
            }
        }
    }
    Queued {
        queued,
        commands,
        failed,
        ..Default::default()
    }
}

/// Queue the restore of the agent stopped as `execution` in `space`, once
/// an execution a daemon restart left unknown in a workspace is proven
/// ended, starting it at once when `now`.
pub(super) async fn queue(
    ctx: &Context,
    space: &PresentSpace,
    execution: &ExecutionId,
    now: bool,
) -> Result<QueueEntry> {
    if let PresentSpace::Workspace(workspace) = space {
        let selector = SpaceSelector::id(&workspace.id);
        let inspection = crate::client::workspaces::inspect(&ctx.paths, selector).await?;
        stale::reconcile(ctx, &inspection, Some(execution)).await?;
    }
    let execution = execution.to_owned();
    crate::client::queue::restore(
        &ctx.paths,
        SpaceSelector::id(space.id()),
        execution,
        terminal::start_environment(),
        now,
    )
    .await
}
