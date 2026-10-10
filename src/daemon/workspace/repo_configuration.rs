//! Local repository configuration belongs to the registration, not its checkout.
use super::Manager;
use crate::{
    config::{
        self, Config, Effective,
        repo::{ConfigLayers, LocalConfig, RepoConfig},
    },
    hooks::{HookDirectory, HookKind},
    model::Workspace,
    protocol::ConfigTarget,
};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

impl Manager {
    pub async fn repository_config(
        &self,
        selector: impl Into<crate::forge::repository::Selector>,
    ) -> Result<LocalConfig> {
        let repo = self.repository(selector).await?;
        Ok(LocalConfig {
            toml: self.local_repository_config(&repo.id).await?,
            repository_id: repo.id,
        })
    }

    pub async fn set_repository_config(
        &self,
        selector: impl Into<crate::forge::repository::Selector>,
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
        selector: impl Into<crate::forge::repository::Selector>,
        changes: &[config::edit::Change],
    ) -> Result<LocalConfig> {
        // Hold the same gate as imports and removal across read/modify/write.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        let text = self
            .local_repository_config(&repo.id)
            .await?
            .unwrap_or_default();
        let edited = config::edit::apply(&text, changes)?;
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
        let checkout = match kind.directory() {
            HookDirectory::Worktree => workspace.path.clone(),
            HookDirectory::Checkout => self.repository(&workspace.repository_id).await?.path,
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
        let saved_repository_config = self
            .local_repository_config(repository_id)
            .await?
            .map(|text| config::repo::parse(&text).context("parse local repository config"))
            .transpose()?
            .unwrap_or_default();
        Ok(ConfigLayers {
            worktree_file,
            saved_repository_config,
        })
    }

    /// The workspace's settings after every layer: the saved config, the
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

    pub(crate) async fn configured_workspace_environment(
        &self,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let settings = self.workspace_settings(workspace).await?;
        let repository = self.repository(&workspace.repository_id).await?;
        let ports = self.list_ports(Some(&workspace.id)).await?;
        Ok(crate::env::render_configured_environment(
            &settings.env,
            workspace,
            crate::forge::repository::name(&repository),
            &ports,
        ))
    }
}
