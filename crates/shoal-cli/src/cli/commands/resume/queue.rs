//! Queuing the restores of stopped agents, which the daemon starts as the
//! agent limits allow, or at once with `--now`.
use anyhow::{Result, ensure};
use shoal_ids::execution_ids::ExecutionId;

use super::reports::{self, Reporting};
use super::targets::Selected;
use crate::client::output::resume::Queued;
use crate::{
    cli::context::Context, config::permissions::PermissionChange, execution::recovery::Saved,
    model::Space,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::saved::{Queuing, Restore};
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::resume::{permissions, queue::ensure_daemon_starts};

/// Queue the restores of the stopped agents and last sessions of the
/// `selected` spaces this process's scope reaches, or of every such space
/// with any, only `execution`'s when given, and the selected failed
/// launches, under `change` when given, with what the resume does
/// `alongside`; the daemon refuses a space outside the scope before any
/// restore, and starts the agents detached.
pub(super) async fn queue_below(
    ctx: &Context,
    selected: Selected,
    execution: Option<ExecutionId>,
    change: Option<&PermissionChange>,
    now: bool,
    alongside: super::Alongside,
) -> Result<i32> {
    let stopped = match selected.spaces.clone() {
        Some(selectors) => {
            let mut stopped = Vec::new();
            for selector in selectors {
                stopped.push(stopped_agents(ctx, selector, execution.as_ref()).await?);
            }
            stopped
        }
        None => stopped_in_reach(ctx).await?,
    };
    permissions::apply(&ctx.paths, &stopped, &selected.failed_starts, change).await?;
    let mut resumed = alongside.continue_turns(ctx).await;
    super::retry_failed_starts(ctx, &selected, &mut resumed, now).await;
    queue_all(ctx, stopped, resumed, now).await
}

pub(in crate::cli::commands) use shoal_workers::resume::queue::Stopped;

/// Queue the restores of the agents among `stopped`; their workspaces'
/// stopped commands go to them, and those of workspaces without one stay
/// for `shoal resume`.
pub(in crate::cli::commands) async fn queue_stopped(
    ctx: &Context,
    stopped: &Stopped,
) -> Result<Queued> {
    shoal_workers::resume::queue::queue_stopped(&ctx.paths, stopped, &Reporting(ctx)).await
}

/// Queue the restores of the agents among `stopped`, warning when that
/// fails, since `shoal resume` still resumes them.
pub(in crate::cli::commands) async fn restore_stopped(
    ctx: &Context,
    stopped: &Stopped,
) -> Option<Queued> {
    if stopped.is_empty() {
        return None;
    }
    match queue_stopped(ctx, stopped).await {
        Ok(queued) => Some(queued),
        Err(error) => {
            eprintln!("warning: cannot resume the stopped agents; run shoal resume: {error:#}");
            None
        }
    }
}

/// The executions whose stopped work the space `space` saves for `shoal
/// restore`.
pub(in crate::cli::commands) async fn saved_executions(
    ctx: &Context,
    space: &SpaceId,
) -> Result<std::collections::BTreeSet<ExecutionId>> {
    Ok(
        crate::client::saved::read(&ctx.paths, SpaceSelector::id(space))
            .await?
            .execution_ids()
            .into_iter()
            .collect(),
    )
}

/// The stopped agents and last sessions of the workspace `selector` names,
/// only `execution`'s when given, refusing one without any.
async fn stopped_agents(
    ctx: &Context,
    selector: SpaceSelector,
    execution: Option<&ExecutionId>,
) -> Result<(Space, Saved)> {
    let space = super::present_space(ctx, &selector).await?;
    let mut saved = unattended(ctx, &space).await?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !(saved.agents.is_empty() && saved.sessions.is_empty()),
        "{} has no stopped agent to restore",
        space.name
    );
    Ok((space, saved))
}

/// The stopped agents and last sessions of every space this process's
/// scope reaches but its own that wait in no wrapper, as restore `--all`
/// finds them.
async fn stopped_in_reach(ctx: &Context) -> Result<Vec<(Space, Saved)>> {
    let mut stopped = super::saved::stopped(ctx, &Selected::default()).await?;
    for (_, saved) in &mut stopped {
        keep_unattended(saved);
    }
    stopped.retain(|(_, saved)| !(saved.agents.is_empty() && saved.sessions.is_empty()));
    Ok(stopped)
}

/// The stopped agents and last sessions of `space` that wait in no wrapper,
/// which a scoped process queues.
async fn unattended(ctx: &Context, space: &Space) -> Result<Saved> {
    let mut saved = saved(ctx, space).await?;
    keep_unattended(&mut saved);
    Ok(saved)
}

/// Keep what `saved` holds of agents and last sessions that wait in no
/// wrapper; its stopped commands go to the restored agents.
fn keep_unattended(saved: &mut Saved) {
    saved.retain_unattended();
    saved.commands.clear();
}

/// Restore the stopped agents of the space `selector` names, only
/// `execution`'s when given, as [`Saved::restore`] decides for `queuing`,
/// making `change` when given.
pub(super) async fn queue_space(
    ctx: &Context,
    selector: SpaceSelector,
    execution: Option<ExecutionId>,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let space = super::present_space(ctx, &selector).await?;
    let saved = shoal_workers::resume::space::saved(&ctx.paths, &space, execution.as_ref()).await?;
    let now = match saved.restore(queuing) {
        Restore::Continue => return super::space::now(ctx, selector, execution, change).await,
        Restore::Queue { now } => now,
    };
    let stopped = vec![(space, saved)];
    if matches!(queuing, Queuing::Daemon { .. }) {
        ensure_daemon_starts(&ctx.paths, &stopped).await?;
    }
    permissions::apply(&ctx.paths, &stopped, &[], change).await?;
    queue_all(ctx, stopped, Queued::default(), now).await
}

/// What `workspace` saved.
async fn saved(ctx: &Context, space: &Space) -> Result<Saved> {
    crate::client::saved::read(&ctx.paths, SpaceSelector::id(&space.id)).await
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once regardless of the limits when `now`, and report
/// the commands of workspaces without one, going on after one fails, with
/// the reopened spaces, continued turns and failures `resumed`
/// reports already.
pub(super) async fn queue_all(
    ctx: &Context,
    stopped: Vec<(Space, Saved)>,
    resumed: Queued,
    now: bool,
) -> Result<i32> {
    let mut queued = queue_each(ctx, stopped, now).await;
    let mut failed = resumed.failed;
    failed.append(&mut queued.failed);
    queued.failed = failed;
    queued.queued.splice(0..0, resumed.queued);
    queued.reopened = resumed.reopened;
    queued.continued = resumed.continued;
    reports::queued(ctx, &queued)?;
    Ok(if queued.failed.is_empty() { 0 } else { 1 })
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once when `now`, telling the user of what fails as
/// [`shoal_workers::resume::queue::queue_each`] goes.
pub(super) async fn queue_each(ctx: &Context, stopped: Vec<(Space, Saved)>, now: bool) -> Queued {
    let reporting = Reporting(ctx);
    shoal_workers::resume::queue::queue_each(&ctx.paths, stopped, now, &reporting).await
}
