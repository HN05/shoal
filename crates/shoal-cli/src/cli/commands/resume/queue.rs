//! Queuing the restores of the agents a move stopped, which the daemon
//! starts as the agent limits allow.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;

use super::reports::Reporting;
use crate::cli::context::Context;
use crate::client::output::resume::Queued;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) use shoal_workers::resume::queue::Stopped;

/// Queue the restores of the agents among `stopped`; their workspaces'
/// stopped commands go to them, and those of workspaces without one stay
/// for `shoal resume`.
async fn queue_stopped(ctx: &Context, stopped: &Stopped) -> Result<Queued> {
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
