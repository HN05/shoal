//! Local repository configuration belongs to the registration, not its checkout.
use super::Manager;
use crate::{
    config::{
        self, Effective,
        named_commands::CommandLayers,
        repo::{ConfigLayers, Hooks, LocalConfig, RepoConfig},
    },
    model::Workspace,
    protocol::ConfigTarget,
};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

impl Manager {
    pub async fn command_layers(&self, selector: &str) -> Result<CommandLayers> {
        let workspace = self.workspace(selector).await?;
        let worktree_file = config::repo::load(&workspace.path)?.commands;
        let saved_repository_config = self
            .local_repository_config(&workspace.repository_id)
            .await?
            .map(|text| {
                config::repo::parse(&text)
                    .context("parse local repository config")
                    .map(|config| config.commands)
            })
            .transpose()?
            .unwrap_or_default();
        Ok(CommandLayers {
            worktree_file,
            saved_repository_config,
        })
    }

    pub async fn repository_config(&self, selector: &str) -> Result<LocalConfig> {
        let repo = self.repository(selector).await?;
        Ok(LocalConfig {
            toml: self.local_repository_config(&repo.id).await?,
            repository_id: repo.id,
        })
    }

    pub async fn set_repository_config(
        &self,
        selector: &str,
        toml: Option<String>,
    ) -> Result<LocalConfig> {
        if let Some(text) = &toml {
            config::repo::parse(text).context("invalid local repository config")?;
        }
        // Serialize changes with registration/removal and retain config on failed cleanup.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        self.save_repository_config(repo.id, toml).await
    }

    pub async fn edit_repository_config(
        &self,
        selector: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<LocalConfig> {
        // Hold the same gate as imports and removal across read/modify/write.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        let text = self
            .local_repository_config(&repo.id)
            .await?
            .unwrap_or_default();
        let edited = crate::config::edit::edit(&text, key, value)?;
        config::repo::parse(&edited).context("invalid local repository config")?;
        self.save_repository_config(repo.id, Some(edited)).await
    }

    async fn save_repository_config(
        &self,
        repository_id: String,
        toml: Option<String>,
    ) -> Result<LocalConfig> {
        self.store
            .run(move |db| {
                if let Some(text) = &toml {
                    db.execute(
                        "INSERT INTO repository_configs(repository_id,toml) VALUES (?1,?2)
                     ON CONFLICT(repository_id) DO UPDATE SET toml=excluded.toml",
                        params![repository_id, text],
                    )?;
                } else {
                    db.execute(
                        "DELETE FROM repository_configs WHERE repository_id=?1",
                        [&repository_id],
                    )?;
                }
                Ok(LocalConfig {
                    repository_id,
                    toml,
                })
            })
            .await
    }

    async fn local_repository_config(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT toml FROM repository_configs WHERE repository_id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    pub async fn workspace_hooks(&self, selector: &str) -> Result<Hooks> {
        let workspace = self.workspace(selector).await?;
        Ok(self
            .workspace_config(&workspace)
            .await?
            .hooks(&workspace.path))
    }

    /// The workspace's repository config: the saved local config layered per
    /// option over the worktree's own `.shoal.toml`.
    pub(crate) async fn workspace_config(&self, workspace: &Workspace) -> Result<RepoConfig> {
        let file = config::repo::load(&workspace.path)?;
        Ok(self
            .config_layers(&workspace.repository_id, file)
            .await?
            .resolve())
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
        let saved_repository_config = self
            .local_repository_config(repository_id)
            .await?
            .map(|text| config::repo::parse(&text).context("parse local repository config"))
            .transpose()?
            .unwrap_or_default();
        let layers = ConfigLayers {
            worktree_file,
            saved_repository_config,
        };
        // Each layer is valid alone; the layered names must agree too.
        let mut resources = layers.worktree_file.resources.clone();
        resources.extend(layers.saved_repository_config.resources.clone());
        let mut resource_pools = layers.worktree_file.resource_pools.clone();
        resource_pools.extend(layers.saved_repository_config.resource_pools.clone());
        crate::daemon::resources::definitions(&resources, &resource_pools)
            .context("layered repository config")?;
        Ok(layers)
    }

    /// The workspace's settings after every layer: the saved config, the
    /// worktree file and the global config.
    pub(crate) async fn workspace_settings(&self, workspace: &Workspace) -> Result<Effective> {
        self.config
            .effective(&self.workspace_config(workspace).await?)
    }
}
