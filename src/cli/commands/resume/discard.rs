//! `shoal resume --discard`: forgetting stopped agents, commands and last
//! sessions instead of restoring them, and removing stopped chats.
use anyhow::Result;

use super::chats;
use crate::client::assignments;
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::DiscardedWork;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::Saved,
    model::{Chat, Workspace},
};

/// Forget the workspace's stopped work and last sessions, only
/// `execution`'s when given.
pub(super) async fn discard_work(
    ctx: &Context,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    let discarded = assignments::discard_stopped_work(&ctx.paths, workspace, execution).await?;
    let forgotten = DiscardedWork {
        discarded: discarded.executions,
        chats: Vec::new(),
        failed: Vec::new(),
    };
    ctx.show(&forgotten, |_| {
        outln!(
            "Discarded stopped agents and commands for {}",
            discarded.workspace
        )
    })?;
    Ok(0)
}

/// Remove the stopped `chats` and forget every workspace's stopped agents and
/// commands, going on after one fails.
pub(super) async fn discard_each(
    ctx: &Context,
    stopped: &[(Workspace, Saved)],
    chats: &[Chat],
) -> Result<i32> {
    let (chats, mut failed) = chats::discard_each(ctx, chats).await;
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for (workspace, saved) in stopped {
        match discard_saved(ctx, workspace, saved).await {
            Ok(executions) => {
                ids.extend(executions);
                names.push(workspace.name.as_str());
            }
            Err(error) => failed.push(ui::failure(ctx, "discard", &workspace.name, &error)),
        }
    }
    let discarded = DiscardedWork {
        discarded: ids,
        chats,
        failed,
    };
    ctx.show(&discarded, |_| {
        if !names.is_empty() {
            outln!(
                "Discarded stopped agents and commands for {}",
                names.join(", ")
            );
        }
    })?;
    Ok(if discarded.failed.is_empty() { 0 } else { 1 })
}

/// Forget `workspace`'s stopped records each by itself, leaving agents a
/// wrapper waits to restore, and return the executions forgotten.
async fn discard_saved(
    ctx: &Context,
    workspace: &Workspace,
    saved: &Saved,
) -> Result<Vec<ExecutionId>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let workspace = workspace.id.clone();
        let discarded = assignments::discard_stopped_work(
            &ctx.paths,
            SpaceSelector::id(&workspace),
            Some(execution),
        )
        .await?;
        ids.extend(discarded.executions);
    }
    Ok(ids)
}
