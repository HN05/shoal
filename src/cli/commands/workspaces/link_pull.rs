//! Linking the PR a workspace was opened on.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
    },
    model::Workspace,
    protocol::ItemMethod,
};

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
    Ok(client::inspect(&ctx.paths, workspace.id).await?.workspace)
}

/// Link PR `url` as the one workspace `id` was opened on; whether it did,
/// a refused link having warned.
pub(super) async fn link_or_warn(ctx: &Context, id: &str, url: &str) -> bool {
    let linked = request::<()>(
        &ctx.paths,
        ItemMethod::SetPr {
            space: id.to_owned(),
            action: crate::daemon::links::pr::Action::Watch {
                url: url.to_owned(),
            },
        },
    )
    .await;
    if let Err(error) = &linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
    }
    linked.is_ok()
}
