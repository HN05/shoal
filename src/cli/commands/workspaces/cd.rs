//! Moving the shell into a workspace.
use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{client, context::Context, recent, ui},
    shell,
};

pub(in crate::cli::commands) async fn cd(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => ui::explicit_workspace(ctx, workspace).await?,
        None => ui::workspace_picker(ctx).await?,
    };
    let inspection = client::inspect(&ctx.paths, workspace).await?;
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
    ctx.emit(&path.display().to_string(), json!({"path": path}))?;
    shell::navigate(path, ctx.json)
}
