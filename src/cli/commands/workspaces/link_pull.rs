//! Linking the PR a workspace was opened on.
use anyhow::Result;

use crate::{cli::context::Context, model::Workspace};

/// Link the PR a workspace was opened on and return the workspace with that
/// link. The workspace already exists, so a refused link only warns.
pub(in crate::cli::commands) async fn link_pull(
    ctx: &Context,
    workspace: Workspace,
    url: String,
) -> Result<Workspace> {
    if !link_or_warn(ctx, &workspace.id, &url).await {
        return Ok(workspace);
    }
    Ok(crate::client::workspaces::inspect(&ctx.paths, workspace.id)
        .await?
        .workspace)
}

/// Link PR `url` as the one workspace `id` was opened on; whether it did,
/// a refused link having warned.
pub(super) async fn link_or_warn(ctx: &Context, id: &str, url: &str) -> bool {
    let linked = crate::client::items::watch_pr(&ctx.paths, id.to_owned(), url.to_owned()).await;
    if let Err(error) = &linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
    }
    linked.is_ok()
}
