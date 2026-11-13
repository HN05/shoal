//! Linking the PR a workspace was opened on.
use anyhow::Result;

use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    forge::items::{
        link::LinkKind,
        reference::{ItemInput, ItemRef},
    },
    model::{Relation, Workspace},
    paths::CommandPaths,
};

/// Link the PR a workspace was opened on and return the workspace with that
/// link. The workspace already exists, so a refused link only warns.
pub async fn link(paths: &CommandPaths, workspace: Workspace, url: ItemRef) -> Result<Workspace> {
    if !link_or_warn(paths, &workspace.id, &url.into()).await {
        return Ok(workspace);
    }
    Ok(
        crate::client::workspaces::inspect(paths, SpaceSelector::id(&workspace.id))
            .await?
            .workspace,
    )
}

/// Link PR `url`, a number or URL, as the one workspace `id` was opened on;
/// whether it did, a refused link having warned.
pub(crate) async fn link_or_warn(paths: &CommandPaths, id: &SpaceId, url: &ItemInput) -> bool {
    let space = SpaceSelector::id(id);
    let (relation, kind) = (Relation::Assigned, LinkKind::Pr);
    let linked = crate::client::items::link(paths, &space, relation, kind, url.clone(), None).await;
    if let Err(error) = &linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
    }
    linked.is_ok()
}
