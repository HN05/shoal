//! Recording a workspace's assignment as done, and withdrawing that.
use anyhow::Result;
use serde_json::json;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    model::Completion,
    protocol::{AssignmentMethod, DemoMethod},
};

pub(in crate::cli::commands) async fn undone(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let withdrawn: Completion =
        request(&ctx.paths, AssignmentMethod::WorkspaceUndone { workspace }).await?;
    ctx.emit(
        "Completion withdrawn; run shoal done when finished.",
        json!({ "withdrawn": withdrawn }),
    )?;
    Ok(0)
}

/// Stop the workspace's demos unless `keep_demos`, then record completion.
pub(in crate::cli::commands) async fn done(
    ctx: &Context,
    workspace: Option<String>,
    cleanup: bool,
    keep_demos: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Before completion, whose cleanup may remove the workspace at once.
    if !keep_demos {
        request::<()>(
            &ctx.paths,
            DemoMethod::StopDemos {
                workspace: workspace.clone(),
                name: None,
            },
        )
        .await?;
    }
    let completion: Completion = request(
        &ctx.paths,
        AssignmentMethod::WorkspaceDone {
            workspace,
            cleanup,
            integrated: false,
        },
    )
    .await?;
    let message = if completion.cleanup {
        "Assignment marked done; cleanup requested. Tracked commands may stop and the workspace may be removed."
    } else {
        "Assignment marked done; no cleanup requested."
    };
    ctx.emit(message, &completion)?;
    Ok(0)
}
