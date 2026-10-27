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
    let linked = request::<()>(
        &ctx.paths,
        ItemMethod::SetPr {
            workspace: workspace.id.clone(),
            action: crate::daemon::links::pr::Action::Watch { url: url.clone() },
        },
    )
    .await;
    if let Err(error) = linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
        return Ok(workspace);
    }
    Ok(client::inspect(&ctx.paths, workspace.id).await?.workspace)
}
