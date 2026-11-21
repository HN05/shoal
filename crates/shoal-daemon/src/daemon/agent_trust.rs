//! The agents' trust entries for a repository's directories, which its
//! removal removes and its move carries to the new directories, in the
//! tools' default configs and wherever the repository's agents kept theirs,
//! and the locations the daemon keeps for them.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use rusqlite::params;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

use super::manager::Manager;
use crate::{
    daemon::{log, notifications::AlertKind, store},
    model::Repository,
};

/// What happened to a repository's directories.
#[derive(Clone)]
pub(super) enum TrustChange {
    /// The repository is removed, with the workspaces at these paths.
    Removed(Vec<PathBuf>),
    /// Its directories moved: each path it had, with the one it has now.
    Moved(Vec<(PathBuf, PathBuf)>),
}

impl TrustChange {
    /// Make the change to the trust entries for `repository` in the configs
    /// under `home` and those of `environments`; each failure comes back as
    /// a warning.
    fn apply(
        &self,
        home: &Path,
        repository: &Repository,
        environments: &[BTreeMap<String, String>],
    ) -> Vec<String> {
        match self {
            Self::Removed(workspace_paths) => crate::agents::forget_repository_trust(
                home,
                repository,
                workspace_paths,
                environments,
            ),
            Self::Moved(moves) => crate::agents::move_repository_trust(home, moves, environments),
        }
    }

    /// What the user reads when the change left entries unchanged.
    fn left_behind(&self, repository: &Repository) -> String {
        let name = repository.display_name();
        match self {
            Self::Removed(_) => format!("Removing repository {name} left agent trust entries"),
            Self::Moved(_) => format!("Moving repository {name} left agent trust entries"),
        }
    }
}

impl Manager {
    /// Make `change` to the agents' trust entries for `repository`'s
    /// directories, in the tools' default configs and in those its agents
    /// ran with, which a removal then forgets. Failures go to the log and
    /// in an alert to the user, never to the caller; a development build
    /// leaves the user's own home to the installed Shoal.
    pub(super) async fn update_agent_trust(&self, repository: &Repository, change: TrustChange) {
        if !crate::build::changes_files_under(&self.paths.home) {
            return;
        }
        let environments = self.agent_environments(&repository.id).await;
        let mut warnings = Vec::new();
        if let Err(error) = &environments {
            warnings.push(format!(
                "where its agents kept their configs is unknown: {error:#}"
            ));
        }
        let kept = environments.as_ref().map(Vec::clone).unwrap_or_default();
        warnings.extend(self.edit_trust(repository, &change, kept).await);
        if matches!(change, TrustChange::Removed(_)) && environments.is_ok() {
            self.forget_agent_environments_logged(repository).await;
        }
        self.report_trust_left(repository, &change, warnings).await;
    }

    /// [`TrustChange::apply`] off the runtime, since it waits for other
    /// Shoal processes' locks on the config files.
    async fn edit_trust(
        &self,
        repository: &Repository,
        change: &TrustChange,
        environments: Vec<BTreeMap<String, String>>,
    ) -> Vec<String> {
        let home = self.paths.home.clone();
        let (repository, change) = (repository.clone(), change.clone());
        tokio::task::spawn_blocking(move || change.apply(&home, &repository, &environments))
            .await
            .unwrap_or_else(|error| vec![format!("the trust update ended: {error}")])
    }

    /// Log each of `warnings` and alert the user to them.
    async fn report_trust_left(
        &self,
        repository: &Repository,
        change: &TrustChange,
        warnings: Vec<String>,
    ) {
        if warnings.is_empty() {
            return;
        }
        let name = repository.display_name();
        for warning in &warnings {
            log!("repository {name}: {warning}");
        }
        let message = format!(
            "{}: {}",
            change.left_behind(repository),
            warnings.join("; ")
        );
        self.notify(None, AlertKind::TrustLeft, message).await;
    }

    /// [`Self::forget_agent_environments`], logging a failure, which only
    /// keeps rows no agent adds to again.
    async fn forget_agent_environments_logged(&self, repository: &Repository) {
        if let Err(error) = self.forget_agent_environments(&repository.id).await {
            let name = repository.display_name();
            log!("repository {name}: its agents' config locations stay recorded: {error:#}");
        }
    }

    /// Keep `environment`, the [`crate::env::AGENT_CONFIG_VARIABLES`] an
    /// agent of the workspace runs with, for the repository's removal or
    /// move to update the agent's trust entries where they point. Other
    /// variables are ignored.
    pub async fn keep_agent_environment(
        &self,
        selector: &SpaceSelector,
        environment: BTreeMap<String, String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.keep_environment(workspace.repository(), environment)
            .await
    }

    /// [`Self::keep_agent_environment`] for a chat agent of the repository.
    pub async fn keep_repository_agent_environment(
        &self,
        repository_id: &RepositoryId,
        environment: BTreeMap<String, String>,
    ) -> Result<()> {
        let repository = self.repository(repository_id).await?;
        self.keep_environment(&repository.id, environment).await
    }

    async fn keep_environment(
        &self,
        repository_id: &RepositoryId,
        mut environment: BTreeMap<String, String>,
    ) -> Result<()> {
        environment.retain(|name, _| crate::env::AGENT_CONFIG_VARIABLES.contains(&name.as_str()));
        if environment.is_empty() {
            return Ok(());
        }
        self.store_agent_environment(repository_id, &environment)
            .await
    }

    /// Record `environment` for the repository once.
    async fn store_agent_environment(
        &self,
        repository_id: &RepositoryId,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let (repository_id, environment) = (
            repository_id.to_owned(),
            serde_json::to_string(environment)?,
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO repository_agent_environments(repository_id,environment) VALUES (?1,?2)",
                    params![repository_id, environment],
                )?;
                Ok(())
            })
            .await
    }

    /// What [`Self::keep_agent_environment`] kept for the repository with
    /// `id`, which may be removed already.
    pub async fn agent_environments(
        &self,
        id: &RepositoryId,
    ) -> Result<Vec<BTreeMap<String, String>>> {
        let id = id.to_owned();
        self.store
            .read(move |tx| {
                let mut statement = tx.prepare(
                    "SELECT environment FROM repository_agent_environments WHERE repository_id=?1 ORDER BY environment",
                )?;
                let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
                rows.map(|environment| Ok(serde_json::from_str(&environment?)?))
                    .collect()
            })
            .await
    }

    /// Forget what [`Self::keep_agent_environment`] kept for the removed
    /// repository with `id`, which no agent can add to again.
    async fn forget_agent_environments(&self, id: &RepositoryId) -> Result<()> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                ensure!(
                    !store::exists(tx, "SELECT 1 FROM repositories WHERE id=?1", [&id])?,
                    "the repository is still registered"
                );
                tx.execute(
                    "DELETE FROM repository_agent_environments WHERE repository_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
