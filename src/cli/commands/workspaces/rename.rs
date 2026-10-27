//! Renaming a workspace's branch.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    model::Workspace,
    protocol::WorkspaceMethod,
};

pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    workspace: Option<String>,
    branch: String,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let renamed = request::<Workspace>(
        &ctx.paths,
        WorkspaceMethod::RenameWorkspace { workspace, branch },
    )
    .await?;
    ctx.show(&renamed, |workspace| {
        outln!(
            "Renamed workspace to {} ({})",
            workspace.name,
            workspace.branch
        )
    })?;
    Ok(0)
}
