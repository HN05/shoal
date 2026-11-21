//! A space's settings as its launches resolve them: the global config read
//! now under the repository layers the daemon has, and the `[env]`
//! variables they give the space's tracked commands, hooks and agents.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context as _, Result};

use crate::{
    client::connection::{CommandPaths, Daemon},
    config::{Config, Effective, repo::ConfigLayers},
    model::Workspace,
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

/// The variables `[env]` and the workspace's temporary directory add to its
/// tracked executions and hooks.
pub async fn configured_environment(
    paths: &CommandPaths,
    workspace: &Workspace,
) -> Result<BTreeMap<String, OsString>> {
    let settings = of(paths, ConfigTarget::Space(SpaceSelector::id(&workspace.id))).await?;
    let repo = crate::client::repositories::list(paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == *workspace.repository())
        .context("workspace repository is missing")?;
    Ok(crate::env::render_configured_environment(
        &settings.env,
        workspace,
        Some(repo.display_name()),
        &crate::client::workspaces::inspect(paths, SpaceSelector::id(&workspace.id))
            .await?
            .ports,
        // Only the daemon creates them; tools create a missing cache, and
        // the daemon gives out hooks with the temporary directory created.
        &crate::caches::locate(&repo, &settings.caches),
        Some(&crate::tmp_dirs::locate(&repo, &workspace.id)),
    ))
}
