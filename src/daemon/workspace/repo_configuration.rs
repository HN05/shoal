//! The configuration layers of a repository's workspaces, and where its
//! local config file lives.
use super::Manager;
use crate::{
    config::{
        self, Config, Effective,
        repo::{ConfigLayers, RepoConfig},
    },
    hooks::{HookDirectory, HookKind},
    model::Workspace,
    protocol::ConfigTarget,
};
use anyhow::Result;
use std::{collections::BTreeMap, path::PathBuf};

impl Manager {
    /// The repository's local config file, which the CLI edits in place;
    /// its directory is reserved when the repository has none yet.
    pub async fn local_config_path(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<PathBuf> {
        let repo = self.repository(selector).await?;
        if let Some(directory) = &repo.workspaces_dir {
            return Ok(config::repo::local_config_path(directory));
        }
        let _git = self.gates.lock_git(&repo.id).await;
        self.ensure_repository_available(&repo.id).await?;
        let directory = self.workspaces_dir(&repo).await?;
        Ok(config::repo::local_config_path(&directory))
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
        // The caller renders the hook's [env], whose caches only the daemon creates.
        crate::caches::directories(&repository, &settings.caches)?;
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

    async fn config_layers(
        &self,
        repository_id: &str,
        worktree_file: RepoConfig,
    ) -> Result<ConfigLayers> {
        let repository = self.repository(repository_id).await?;
        let local_config = match &repository.workspaces_dir {
            Some(directory) => config::repo::load_local(directory)?,
            None => RepoConfig::default(),
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

    pub(crate) async fn configured_workspace_environment(
        &self,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let settings = self.workspace_settings(workspace).await?;
        let repository = self.repository(&workspace.repository_id).await?;
        let ports = self.list_ports(Some(&workspace.id)).await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        Ok(crate::env::render_configured_environment(
            &settings.env,
            workspace,
            crate::repository::name(&repository),
            &ports,
            &caches,
        ))
    }
}
