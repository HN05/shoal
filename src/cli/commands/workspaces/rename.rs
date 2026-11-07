//! Renaming a workspace's branch.
use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Fallback},
};
use crate::client::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    workspace: Option<String>,
    branch: String,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let renamed =
        crate::client::workspaces::rename(&ctx.paths, SpaceSelector::parse(&workspace), branch)
            .await?;
    ctx.show(&renamed, |workspace| {
        outln!(
            "Renamed workspace to {} ({})",
            workspace.name,
            workspace.branch_or_name()
        )
    })?;
    Ok(0)
}
