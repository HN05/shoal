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

/// How `shoal done` records the completion, and what it leaves running.
#[derive(Default)]
pub(in crate::cli::commands) struct Done {
    /// Request cleanup even when `[cleanup.done] enabled` is false.
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits.
    pub integrated: bool,
    pub keep_demos: bool,
    /// The files to hand back, checked before anything is picked or
    /// stopped.
    pub results: Vec<crate::daemon::workspace_files::NewFile>,
}

/// Stop the workspace's demos unless `keep_demos`, then record completion.
pub(in crate::cli::commands) async fn done(
    ctx: &Context,
    workspace: Option<String>,
    options: Done,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Before completion, whose cleanup may remove the workspace at once.
    if !options.keep_demos {
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
            cleanup: options.cleanup,
            integrated: options.integrated,
            results: options.results,
        },
    )
    .await?;
    ctx.emit(&done_message(&completion), &completion)?;
    Ok(0)
}

fn done_message(completion: &Completion) -> String {
    let marked = if completion.integrated {
        "Assignment marked done; integrated elsewhere"
    } else {
        "Assignment marked done"
    };
    if completion.cleanup {
        format!(
            "{marked}; cleanup requested. Tracked commands may stop and the workspace may be removed."
        )
    } else {
        format!("{marked}; no cleanup requested.")
    }
}
