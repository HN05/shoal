//! Moving the shell into a workspace.
use anyhow::Result;

use crate::cli::{
    context::Context,
    recent, shell,
    ui::{self, Acting},
};
use crate::client::output::workspaces::Directory;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn cd(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => ui::explicit_workspace(ctx, workspace).await?,
        // The shell goes on in the worktree on this machine.
        None => SpaceSelector::Id(ui::workspace_picker(ctx, &entering()).await?),
    };
    let workspace = crate::client::spaces::enter(workspace)
        .send(&ctx.paths)
        .await?;
    recent::record_move(ctx, &SpaceSelector::id(&workspace.id)).await;
    navigate(ctx, workspace.workdir.path())?;
    Ok(0)
}

/// Report the destination and ask the shell wrapper to change directory.
fn navigate(ctx: &Context, path: &std::path::Path) -> Result<()> {
    let directory = Directory {
        path: path.to_owned(),
    };
    ctx.emit(&path.display().to_string(), directory)?;
    shell::navigate(ctx, path)
}

/// What entering a workspace sends for the workspace a picker picks, whose
/// directory only this machine has.
pub(super) fn entering() -> Acting {
    Acting::here(crate::client::spaces::enter)
}
