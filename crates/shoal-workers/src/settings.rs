//! A space's settings as its launches resolve them: the global config read
//! now under the repository layers the daemon has, and the `[env]`
//! variables they give the space's tracked commands, hooks and agents.
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    client::connection::{CommandPaths, Daemon},
    config::{Config, Effective, repo::ConfigLayers},
    model::{PortReservation, Repository, Space, SpacePart},
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

/// The settings that apply to `target` after every layer.
pub async fn of(daemon: &impl Daemon, target: ConfigTarget) -> Result<Effective> {
    Ok(configuration(daemon, target).await?.1)
}

/// The global config read at launch and the settings it yields for `target`.
pub async fn configuration(
    daemon: &impl Daemon,
    target: ConfigTarget,
) -> Result<(Config, Effective)> {
    let layers: ConfigLayers = crate::client::config::config_layers(daemon, target).await?;
    crate::config::configuration(daemon.paths(), &layers)
}

/// The variables `[env]` and the temporary directory of `space`, a
/// workspace or a chat, add to its tracked executions and hooks, as the
/// daemon renders them.
pub async fn configured_environment(
    paths: &CommandPaths,
    space: &Space,
) -> Result<BTreeMap<String, OsString>> {
    let settings = of(paths, ConfigTarget::space(space)).await?;
    let repo = repository(paths, space).await?;
    // Only the daemon creates them; tools create a missing cache, and
    // the daemon gives out hooks with the temporary directory created.
    let caches = (repo.as_ref())
        .map(|repo| crate::caches::locate(repo, &settings.caches))
        .unwrap_or_default();
    Ok(crate::env::render_configured_environment(
        &settings.env,
        space,
        repo.as_ref().map(Repository::display_name),
        &ports(paths, space).await?,
        &caches,
        tmp_dir(space, repo.as_ref()).as_deref(),
    ))
}

/// The registered repository of `space`; none for a chat without one.
async fn repository(paths: &CommandPaths, space: &Space) -> Result<Option<Repository>> {
    let Some(id) = &space.repository_id else {
        return Ok(None);
    };
    let repositories = crate::client::repositories::list(paths).await?;
    let repo = (repositories.into_iter()).find(|repo| repo.id == *id);
    repo.context("the space's repository is missing").map(Some)
}

/// The ports `space` reserved, which only a workspace does.
async fn ports(paths: &CommandPaths, space: &Space) -> Result<Vec<PortReservation>> {
    match space.worktree() {
        Some(_) => Ok(
            crate::client::workspaces::inspect(paths, SpaceSelector::id(&space.id))
                .await?
                .ports,
        ),
        None => Ok(Vec::new()),
    }
}

/// The temporary directory of `space`: a workspace's in its repository's
/// directory, or the one a chat's record names.
fn tmp_dir(space: &Space, repo: Option<&Repository>) -> Option<PathBuf> {
    match (&space.part, repo) {
        (SpacePart::Chat(chat), _) => chat.tmp_dir.clone(),
        (SpacePart::Workspace(_), Some(repo)) => Some(crate::tmp_dirs::locate(repo, &space.id)),
        (SpacePart::Workspace(_), None) => None,
    }
}
