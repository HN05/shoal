//! `shoal restore --discard-agent`: discarding a space's stopped agent, last
//! sessions, stopped commands and failed launch instead of bringing them
//! back, which leaves its files, branch and worktree as they are.
use anyhow::Result;

use super::reports;
use crate::client::assignments;
use crate::client::output::restore::{DiscardedAgents, Reopened};
use crate::{
    cli::{context::Context, ui},
    execution::recovery::Saved,
    model::{QueueEntry, Space},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Discard the space's stopped agent, last sessions and stopped commands,
/// only `execution`'s when given.
pub(super) async fn discard_one(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    let discarded = assignments::discard_stopped_agent(&ctx.paths, space, execution).await?;
    let report = DiscardedAgents {
        discarded: discarded.executions,
        ..DiscardedAgents::default()
    };
    reports::discarded(ctx, &report, &[discarded.space.as_str()])?;
    Ok(0)
}

/// Discard every space's stopped agents and commands and the `launches`
/// that failed to start, going on after one fails, and report it with the
/// workspaces `reopened` for the restore.
pub(super) async fn discard_each(
    ctx: &Context,
    stopped: &[(Space, Saved)],
    launches: &[QueueEntry],
    reopened: Vec<Reopened>,
) -> Result<i32> {
    let mut report = DiscardedAgents {
        reopened,
        ..DiscardedAgents::default()
    };
    let mut names = Vec::new();
    for (space, saved) in stopped {
        match discard_saved(ctx, space, saved).await {
            Ok(executions) => {
                report.discarded.extend(executions);
                names.push(space.name.as_str());
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &space.name, &error)),
        }
    }
    for launch in launches {
        match crate::client::queue::remove(&ctx.paths, launch.id.to_string()).await {
            Ok(entry) => {
                report.launches.push(entry);
                names.push(launch.name.as_str());
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &launch.name, &error)),
        }
    }
    reports::discarded(ctx, &report, &names)?;
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// Discard the stopped records of `space` each by itself, leaving agents a
/// wrapper waits to restore, and return the executions discarded.
async fn discard_saved(ctx: &Context, space: &Space, saved: &Saved) -> Result<Vec<ExecutionId>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let selector = SpaceSelector::id(&space.id);
        let discarded =
            assignments::discard_stopped_agent(&ctx.paths, selector, Some(execution)).await?;
        ids.extend(discarded.executions);
    }
    Ok(ids)
}
