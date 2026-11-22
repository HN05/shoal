//! Renaming a workspace's branch.
use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Acting, Fallback},
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn rename_branch(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    branch: String,
) -> Result<i32> {
    let renaming = {
        let branch = branch.clone();
        Acting::request(move |space| {
            crate::client::workspaces::rename_branch(space, branch.clone())
        })
    };
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&renaming));
    let workspace = workspace.await?;
    let renamed = (crate::client::workspaces::rename_branch(workspace.clone(), branch))
        .send(&ctx.paths)
        .await?;
    ctx.show(&renamed, |workspace| {
        outln!(
            "Renamed the branch of {} to {}",
            workspace.label(),
            workspace.branch_or_name()
        )
    })?;
    Ok(0)
}
