//! A workspace's `[env]` variables, resolved through the daemon, as tracked
//! commands, hooks and agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context as _, Result};

use crate::{
    client::space_names::SpaceSelector, model::Workspace, paths::CommandPaths,
    protocol::ConfigTarget, repository,
};

/// The variables `[env]` and the workspace's temporary directory add to its
/// tracked executions and hooks.
pub async fn configured_environment(
    paths: &CommandPaths,
    workspace: &Workspace,
) -> Result<BTreeMap<String, OsString>> {
    let settings = crate::client::workspaces::settings(
        paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let repo = crate::client::repositories::list(paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("workspace repository is missing")?;
    Ok(super::render_configured_environment(
        &settings.env,
        workspace,
        repository::name(&repo),
        &crate::client::workspaces::inspect(paths, SpaceSelector::id(&workspace.id))
            .await?
            .ports,
        // Only the daemon creates them; tools create a missing cache, and
        // the daemon gives out hooks with the temporary directory created.
        &crate::caches::locate(&repo, &settings.caches),
        crate::tmp_dirs::locate(&repo, &workspace.id).as_deref(),
    ))
}
