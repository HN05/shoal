//! Queuing the restores of stopped agents, which the daemon starts as the
//! agent limits allow, or at once.
use anyhow::{Context as _, Result};
use shoal_core::execution::recovery::Saved;
use shoal_ids::execution_ids::ExecutionId;

use super::{Resuming, commands, stale};
use crate::{
    client::{connection::CommandPaths, output::resume::Queued},
    model::{QueueEntry, Space},
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

/// Refuse, before any is queued, the restores of `stopped` agents that the
/// daemon would start from the background when no host can take them there;
/// a waiting wrapper restores its agent where it stopped.
pub async fn ensure_daemon_starts(paths: &CommandPaths, stopped: &[(Space, Saved)]) -> Result<()> {
    for (space, saved) in stopped {
        ensure_daemon_starts_in(paths, space, saved).await?;
    }
    Ok(())
}

/// Refuse the restores of the agents `saved` keeps of `space` that the
/// daemon would start from the background when no host can take them
/// there.
pub async fn ensure_daemon_starts_in(
    paths: &CommandPaths,
    space: &Space,
    saved: &Saved,
) -> Result<()> {
    if !saved.restores_outside_wrappers() {
        return Ok(());
    }
    let target = ConfigTarget::Space(SpaceSelector::id(&space.id));
    let settings = crate::settings::of(paths, target).await?;
    crate::terminal::queued::ensure_background(&settings).with_context(|| {
        format!(
            "cannot restore the agents of {} in the background; pass --focus to restore one here",
            space.name
        )
    })
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once when `now`, and report the commands of workspaces
/// without one that restores outside a waiting wrapper, going on after one
/// fails, which `resuming` tells.
pub async fn queue_each(
    paths: &CommandPaths,
    stopped: Vec<(Space, Saved)>,
    now: bool,
    resuming: &impl Resuming,
) -> Queued {
    let mut queued = Vec::new();
    let mut commands = Vec::new();
    let mut failed = Vec::new();
    for (space, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            match commands::report_claimed(resuming, &space, &mut saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(resuming.failed(&space.name, &error)),
            }
        }
        for saved in saved.agents.iter().chain(&saved.sessions) {
            match queue(paths, &space, &saved.id, now).await {
                Ok(entry) => queued.push(entry),
                Err(error) => failed.push(resuming.failed(&space.name, &error)),
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
pub async fn queue(
    paths: &CommandPaths,
    space: &Space,
    execution: &ExecutionId,
    now: bool,
) -> Result<QueueEntry> {
    if space.worktree().is_some() {
        let selector = SpaceSelector::id(&space.id);
        let inspection = crate::client::workspaces::inspect(selector)
            .send(paths)
            .await?;
        stale::reconcile(paths, &inspection, Some(execution)).await?;
    }
    let execution = execution.to_owned();
    crate::client::queue::restore(
        paths,
        SpaceSelector::id(&space.id),
        execution,
        crate::terminal::start_environment(),
        now,
    )
    .await
}
