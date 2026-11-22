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
    let renaming = renaming();
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

/// What renaming a workspace's branch sends for the workspace a picker
/// picks; the new name decides nothing of which workspaces it reaches.
pub(super) fn renaming() -> Acting {
    Acting::request(|space| crate::client::workspaces::rename_branch(space, String::new()))
}
