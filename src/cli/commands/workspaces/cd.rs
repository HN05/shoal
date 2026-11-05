//! Moving the shell into a workspace.
use anyhow::{Result, ensure};

use crate::client::output::workspaces::Directory;
use crate::{
    cli::{context::Context, recent, ui},
    shell,
};

pub(in crate::cli::commands) async fn cd(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => ui::explicit_workspace(ctx, workspace).await?,
        None => ui::workspace_picker(ctx).await?,
    };
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace).await?;
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    recent::record_move(ctx, &inspection.workspace.id).await;
    navigate(ctx, &inspection.workspace.path)?;
    Ok(0)
}

/// Report the destination and ask the shell wrapper to change directory.
fn navigate(ctx: &Context, path: &std::path::Path) -> Result<()> {
    let directory = Directory {
        path: path.to_owned(),
    };
    ctx.emit(&path.display().to_string(), directory)?;
    shell::navigate(path, ctx.json)
}
