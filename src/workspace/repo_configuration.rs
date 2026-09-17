//! Local repository configuration belongs to the registration, not its checkout.
use super::Manager;
use crate::{
    model::Workspace,
    repo_config::{self, LocalConfig, RepoConfig},
};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

impl Manager {
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
            repo_config::parse(text).context("invalid local repository config")?;
        }
        // Serialize changes with registration/removal and retain config on failed cleanup.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        self.store
            .run(move |db| {
                if let Some(text) = &toml {
                    db.execute(
                        "INSERT INTO repository_configs(repository_id,toml) VALUES (?1,?2)
                     ON CONFLICT(repository_id) DO UPDATE SET toml=excluded.toml",
                        params![repo.id, text],
                    )?;
                } else {
                    db.execute(
                        "DELETE FROM repository_configs WHERE repository_id=?1",
                        [&repo.id],
                    )?;
                }
                Ok(LocalConfig {
                    repository_id: repo.id,
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

    /// The effective repository config: the locally saved override, or the
    /// worktree's own `.shoal.toml`.
    pub(crate) async fn workspace_config(&self, workspace: &Workspace) -> Result<RepoConfig> {
        match self
            .local_repository_config(&workspace.repository_id)
            .await?
        {
            Some(text) => repo_config::parse(&text).context("parse local repository config"),
            None => repo_config::load(&workspace.path),
        }
    }
}
