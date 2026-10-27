//! A workspace's diff and conflicts against its base.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    execution, git,
    model::{ConflictCheck, DiffBase},
    protocol::WorkspaceMethod,
};

pub(in crate::cli::commands) async fn diff(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let base = request::<DiffBase>(&ctx.paths, WorkspaceMethod::DiffBase { workspace }).await?;
    execution::run(
        &ctx.paths,
        base.workspace_id,
        git::diff_command(&base.commit),
        None,
    )
    .await
}

pub(in crate::cli::commands) async fn conflicts(
    ctx: &Context,
    workspace: Option<String>,
    target: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let check =
        request::<ConflictCheck>(&ctx.paths, WorkspaceMethod::Conflicts { workspace, target })
            .await?;
    ctx.show(&check, |check| {
        if check.conflicts {
            outln!("Conflicts with {}:", check.target);
            for file in &check.files {
                outln!("  {file}");
            }
        } else {
            outln!("No conflicts with {}", check.target);
        }
    })?;
    Ok(i32::from(check.conflicts))
}
