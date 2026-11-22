//! Renaming a workspace's branch.
use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Fallback},
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn rename_branch(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    branch: String,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
    )
    .await?;
    let renamed =
        crate::client::workspaces::rename_branch(&ctx.paths, workspace.clone(), branch).await?;
    ctx.show(&renamed, |workspace| {
        outln!(
            "Renamed the branch of {} to {}",
            workspace.label(),
            workspace.branch_or_name()
        )
    })?;
    Ok(0)
}
