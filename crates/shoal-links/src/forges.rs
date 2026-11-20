//! The forge of the repository a checkout, a space or an item names.
use std::path::Path;

use anyhow::{Context, Result};
use shoal_core::repository::remote_url_from_path;
use shoal_forge_types::{ForgeRepo, items::reference::ItemInput};
use shoal_protocol::model::Repository;

use crate::{daemon::Forges, space::LinkSpace};

/// The forge of the `origin` of the checkout or worktree at `path`.
pub async fn origin(daemon: &dyn Forges, path: &Path) -> Result<ForgeRepo> {
    let remote = remote_url_from_path(path)
        .await?
        .context("forge actions need an origin remote")?;
    daemon.resolve_forge(&remote).await
}

/// The forge of the space's `origin`.
pub async fn of_space(daemon: &dyn Forges, space: &LinkSpace) -> Result<ForgeRepo> {
    origin(daemon, space.origin_path()).await
}

/// The forge of the registered checkout's `origin`.
pub async fn of_checkout(daemon: &dyn Forges, repository: &Repository) -> Result<ForgeRepo> {
    origin(daemon, &repository.path).await
}

/// The repository `named` names, as OWNER/NAME on the forge of `own` or
/// as a repository URL on any forge, or `own` without one.
pub async fn named(daemon: &dyn Forges, own: ForgeRepo, named: Option<&str>) -> Result<ForgeRepo> {
    match named {
        Some(named) => daemon.resolve_named(&own, named).await,
        None => Ok(own),
    }
}

/// The repository the issue, PR or CI run URL `input` names, or none for a
/// number.
pub async fn of_input(daemon: &dyn Forges, input: &ItemInput) -> Result<Option<ForgeRepo>> {
    Ok(match input {
        ItemInput::Url(item) => Some(daemon.item_forge(item).await?),
        ItemInput::Number(_) => None,
    })
}
