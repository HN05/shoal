//! `shoal resume --discard`: forgetting stopped agents, commands and last
//! sessions instead of restoring them.
use anyhow::Result;

use super::reports;
use crate::client::assignments;
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::DiscardedWork;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::Saved,
    model::PresentSpace,
};

/// Forget the space's stopped work and last sessions, only `execution`'s
/// when given.
pub(super) async fn discard_work(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    let discarded = assignments::discard_stopped_work(&ctx.paths, space, execution).await?;
    let forgotten = DiscardedWork {
        discarded: discarded.executions,
        failed: Vec::new(),
    };
    reports::discarded(ctx, &forgotten, &[discarded.space.as_str()])?;
    Ok(0)
}

/// Forget every space's stopped agents and commands, going on after one
/// fails.
pub(super) async fn discard_each(ctx: &Context, stopped: &[(PresentSpace, Saved)]) -> Result<i32> {
    let mut failed = Vec::new();
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for (space, saved) in stopped {
        match discard_saved(ctx, space, saved).await {
            Ok(executions) => {
                ids.extend(executions);
                names.push(space.name().as_str());
            }
            Err(error) => failed.push(ui::failure(ctx, "discard", space.name(), &error)),
        }
    }
    let discarded = DiscardedWork {
        discarded: ids,
        failed,
    };
    reports::discarded(ctx, &discarded, &names)?;
    Ok(if discarded.failed.is_empty() { 0 } else { 1 })
}

/// Forget the stopped records of `space` each by itself, leaving agents a
/// wrapper waits to restore, and return the executions forgotten.
async fn discard_saved(
    ctx: &Context,
    space: &PresentSpace,
    saved: &Saved,
) -> Result<Vec<ExecutionId>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let selector = SpaceSelector::id(space.id());
        let discarded =
            assignments::discard_stopped_work(&ctx.paths, selector, Some(execution)).await?;
        ids.extend(discarded.executions);
    }
    Ok(ids)
}
