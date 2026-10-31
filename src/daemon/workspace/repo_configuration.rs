//! The configuration layers of a repository's workspaces, and where its
//! local config file lives.
use super::Manager;
use crate::{
    config::{
        self, Config, Effective,
        repo::{ConfigLayers, RepoConfig},
    },
    hooks::{HookDirectory, HookKind},
    model::{LocalConfigEdit, Workspace},
    protocol::ConfigTarget,
};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, path::PathBuf};

impl Manager {
    /// The repository's local config file; its directory is reserved when
    /// the repository has none yet.
    pub async fn local_config_path(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<PathBuf> {
        let repo = self.repository(selector).await?;
        let directory = match &repo.workspaces_dir {
            Some(directory) => directory.clone(),
            None => {
                let _git = self.gates.lock_git(&repo.id).await;
                self.ensure_repository_available(&repo.id).await?;
                self.workspaces_dir(&repo).await?
            }
        };
        self.local_config_outside_checkouts(&directory).await
    }

    /// Apply `changes` to the repository's local config, saving only a valid
    /// result. It holds the gate moves and removal take, so the file stays
    /// where the edit found it.
    pub async fn edit_local_config(
        &self,
        selector: impl Into<crate::repository::Selector>,
        changes: Vec<config::edit::Change>,
    ) -> Result<LocalConfigEdit> {
        let _registry = self.gates.lock_registry().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        let directory = {
            let _git = self.gates.lock_git(&repo.id).await;
            self.workspaces_dir(&repo).await?
        };
        let path = self.local_config_outside_checkouts(&directory).await?;
        let (config, backup) = tokio::task::spawn_blocking(move || {
            config::file::edit(path, &changes, |edited| {
                config::repo::parse(edited)
                    .map(drop)
                    .context("invalid edited local config")
            })
        })
        .await??;
        Ok(LocalConfigEdit { config, backup })
    }

    /// [`Self::workspace_hook`] of the workspace `selector` names.
    pub(crate) async fn selected_workspace_hook(
        &self,
        selector: &str,
        kind: HookKind,
    ) -> Result<Option<std::path::PathBuf>> {
        let workspace = self.workspace(selector).await?;
        self.workspace_hook(&workspace, kind).await
    }

    /// A hook's executable from the resolved settings, in the directory it
    /// runs from.
    pub(crate) async fn workspace_hook(
        &self,
        workspace: &Workspace,
        kind: HookKind,
    ) -> Result<Option<std::path::PathBuf>> {
        let settings = self.workspace_settings(workspace).await?;
        let Some(command) = kind.command(&settings) else {
            return Ok(None);
        };
        let repository = self.repository(&workspace.repository_id).await?;
        // The caller renders the hook's [env], whose caches only the daemon
        // creates, with the temporary directory, which is gone after removal.
        crate::caches::directories(&repository, &settings.caches)?;
        if !matches!(kind, HookKind::PostRemove) {
            crate::tmp_dirs::directory(&repository, &workspace.id)?;
        }
        let checkout = match kind.directory() {
            HookDirectory::Worktree => workspace.path.clone(),
            HookDirectory::Checkout => repository.path,
        };
        Ok(Some(
            kind.directory()
                .path(&workspace.path, &checkout)
                .join(command),
        ))
    }

    /// [`Self::workspace_settings`] with the template files beside the global
    /// config, which a review's instructions read, over the daemon's current
    /// global config.
    pub(crate) async fn workspace_settings_with_templates(
        &self,
        workspace: &Workspace,
    ) -> Result<Effective> {
        let layers = self.workspace_layers(workspace).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    async fn workspace_layers(&self, workspace: &Workspace) -> Result<ConfigLayers> {
        let file = config::repo::load(&workspace.path)?;
        self.config_layers(&workspace.repository_id, file).await
    }

    /// The repository layer for `target`; a repository's file is the one in
    /// its registered checkout.
    pub async fn config_layers_for(&self, target: ConfigTarget) -> Result<ConfigLayers> {
        let (repository_id, worktree_file) = match target {
            ConfigTarget::Workspace(selector) => {
                let workspace = self.workspace(&selector).await?;
                let file = config::repo::load(&workspace.path)?;
                (workspace.repository_id, file)
            }
            ConfigTarget::Repository(selector) => {
                let repo = self.repository(&selector).await?;
                let file = config::repo::load(&repo.path)?;
                (repo.id, file)
            }
        };
        self.config_layers(&repository_id, worktree_file).await
    }

    /// The local config file in the repository directory `directory`, unless
    /// a checkout or worktree lies where that file's directory belongs.
    async fn local_config_outside_checkouts(&self, directory: &std::path::Path) -> Result<PathBuf> {
        if let Some(occupant) = self.local_config_occupant(directory).await? {
            anyhow::bail!(
                "{} is a checkout where the local config belongs; register it in another directory",
                occupant.display()
            );
        }
        Ok(config::repo::local_config_path(directory))
    }

    /// A checkout or worktree, recorded or not, that lies where the local
    /// config of the repository directory `directory` belongs: its tracked
    /// files are never local settings.
    async fn local_config_occupant(&self, directory: &std::path::Path) -> Result<Option<PathBuf>> {
        let local = directory.join(config::repo::LOCAL_DIR);
        if std::fs::symlink_metadata(local.join(".git")).is_ok() {
            return Ok(Some(local));
        }
        self.store
            .run(move |db| crate::daemon::store::checkout_within(db, &local))
            .await
    }

    async fn config_layers(
        &self,
        repository_id: &str,
        worktree_file: RepoConfig,
    ) -> Result<ConfigLayers> {
        let repository = self.repository(repository_id).await?;
        // Registration and workspace creation keep checkouts out of the
        // local config's directory, so one there predates the local config,
        // which therefore has nothing to hold, and edits refuse.
        let local_config = match &repository.workspaces_dir {
            Some(directory) if self.local_config_occupant(directory).await?.is_none() => {
                config::repo::load_local(directory)?
            }
            _ => RepoConfig::default(),
        };
        Ok(ConfigLayers {
            worktree_file,
            local_config,
        })
    }

    /// The workspace's settings after every layer: the local config, the
    /// worktree file and the global config.
    pub(crate) async fn workspace_settings(&self, workspace: &Workspace) -> Result<Effective> {
        self.workspace_settings_from(&self.config(), workspace)
            .await
    }

    /// [`Self::workspace_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub(crate) async fn workspace_settings_from(
        &self,
        config: &Config,
        workspace: &Workspace,
    ) -> Result<Effective> {
        config.resolve(&self.workspace_layers(workspace).await?)
    }

    /// The repository's settings after every layer, with the file in its
    /// registered checkout.
    pub(crate) async fn repository_settings(&self, selector: &str) -> Result<Effective> {
        self.repository_settings_from(&self.config(), selector)
            .await
    }

    /// [`Self::repository_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub(crate) async fn repository_settings_from(
        &self,
        config: &Config,
        selector: &str,
    ) -> Result<Effective> {
        let layers = self
            .config_layers_for(ConfigTarget::Repository(selector.to_owned().into()))
            .await?;
        config.resolve(&layers)
    }

    /// What `[env]` and the workspace's temporary directory, created when
    /// missing, add to the environment of its hooks.
    pub(crate) async fn configured_workspace_environment(
        &self,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        self.rendered_workspace_environment(workspace, true).await
    }

    /// What `[env]` adds to the environment of the post-remove hook, which
    /// runs once the temporary directory is gone and so keeps the inherited
    /// `TMPDIR`.
    pub(crate) async fn removed_workspace_environment(
        &self,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        self.rendered_workspace_environment(workspace, false).await
    }

    async fn rendered_workspace_environment(
        &self,
        workspace: &Workspace,
        with_tmp_dir: bool,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let settings = self.workspace_settings(workspace).await?;
        self.environment_from_settings(workspace, &settings, with_tmp_dir)
            .await
    }

    /// [`Self::configured_workspace_environment`] from `settings` the caller
    /// resolved, so one operation reads one version of the config.
    pub(crate) async fn configured_environment_from(
        &self,
        workspace: &Workspace,
        settings: &Effective,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        self.environment_from_settings(workspace, settings, true)
            .await
    }

    async fn environment_from_settings(
        &self,
        workspace: &Workspace,
        settings: &Effective,
        with_tmp_dir: bool,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let repository = self.repository(&workspace.repository_id).await?;
        let ports = self.list_ports(Some(&workspace.id)).await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = if with_tmp_dir {
            crate::tmp_dirs::directory(&repository, &workspace.id)?
        } else {
            None
        };
        Ok(crate::env::render_configured_environment(
            &settings.env,
            workspace,
            crate::repository::name(&repository),
            &ports,
            &caches,
            tmp_dir.as_deref(),
        ))
    }
}

#[cfg(test)]
mod tests;
