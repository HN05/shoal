//! `shoal resume --forget`: forgetting stopped agents, commands and last
//! sessions instead of restoring them.
use anyhow::Result;

use super::reports;
use crate::client::assignments;
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::ForgottenWork;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::Saved,
    model::PresentSpace,
};

/// Forget the space's stopped work and last sessions, only `execution`'s
/// when given.
pub(super) async fn forget_work(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    let forgotten = assignments::forget_stopped_work(&ctx.paths, space, execution).await?;
    let report = ForgottenWork {
        forgotten: forgotten.executions,
        failed: Vec::new(),
    };
    reports::forgotten(ctx, &report, &[forgotten.space.as_str()])?;
    Ok(0)
}

/// Forget every space's stopped agents and commands, going on after one
/// fails.
pub(super) async fn forget_each(ctx: &Context, stopped: &[(PresentSpace, Saved)]) -> Result<i32> {
    let mut failed = Vec::new();
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for (space, saved) in stopped {
        match forget_saved(ctx, space, saved).await {
            Ok(executions) => {
                ids.extend(executions);
                names.push(space.name().as_str());
            }
            Err(error) => failed.push(ui::failure(ctx, "forget", space.name(), &error)),
        }
    }
    let forgotten = ForgottenWork {
        forgotten: ids,
        failed,
    };
    reports::forgotten(ctx, &forgotten, &names)?;
    Ok(if forgotten.failed.is_empty() { 0 } else { 1 })
}

/// Forget the stopped records of `space` each by itself, leaving agents a
/// wrapper waits to restore, and return the executions forgotten.
async fn forget_saved(
    ctx: &Context,
    space: &PresentSpace,
    saved: &Saved,
) -> Result<Vec<ExecutionId>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let selector = SpaceSelector::id(space.id());
        let forgotten =
            assignments::forget_stopped_work(&ctx.paths, selector, Some(execution)).await?;
        ids.extend(forgotten.executions);
    }
    Ok(ids)
}
