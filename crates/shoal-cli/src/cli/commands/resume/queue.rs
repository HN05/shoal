//! Restoring the agents a move stopped, through their space's daemon.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;

use super::reports::Reporting;
use crate::cli::context::Context;
use crate::client::output::resume::Queued;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transfer::StoppedExecutions;

/// A failure only warns, since `shoal resume` still resumes them.
pub(in crate::cli::commands) async fn restore_stopped(
    ctx: &Context,
    stopped: &StoppedExecutions,
) -> Option<Queued> {
    if stopped.is_empty() {
        return None;
    }
    let reporting = Reporting(ctx);
    match shoal_workers::resume::stopped::restore(&ctx.paths, stopped, &reporting).await {
        Ok(queued) => Some(queued),
        Err(error) => {
            eprintln!("warning: cannot resume the stopped agents; run shoal resume: {error:#}");
            None
        }
    }
}

pub(in crate::cli::commands) async fn saved_agents(
    ctx: &Context,
    space: &SpaceId,
) -> Result<std::collections::BTreeSet<ExecutionId>> {
    let saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(space)).await?;
    Ok(saved.agent_ids().cloned().collect())
}
