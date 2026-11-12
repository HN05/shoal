//! Linking the PR a workspace was opened on.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::context::Context,
    forge::items::reference::{ItemInput, ItemRef},
    model::Workspace,
};

/// Link the PR a workspace was opened on and return the workspace with that
/// link. The workspace already exists, so a refused link only warns.
pub(in crate::cli::commands) async fn link_pull(
    ctx: &Context,
    workspace: Workspace,
    url: ItemRef,
) -> Result<Workspace> {
    if !link_or_warn(ctx, &workspace.id, &url.into()).await {
        return Ok(workspace);
    }
    Ok(
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
            .await?
            .workspace,
    )
}

/// Link PR `url`, a number or URL, as the one workspace `id` was opened on;
/// whether it did, a refused link having warned.
pub(super) async fn link_or_warn(ctx: &Context, id: &SpaceId, url: &ItemInput) -> bool {
    let linked =
        crate::client::items::watch_pr(&ctx.paths, SpaceSelector::id(id), url.clone()).await;
    if let Err(error) = &linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
    }
    linked.is_ok()
}
