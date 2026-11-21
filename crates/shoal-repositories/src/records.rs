//! The records of registered repositories: listing them, finding the one a
//! selector names, naming one, and whether one is free of a removal or
//! move.
use anyhow::{Result, ensure};
use rusqlite::params;
use shoal_core::repository::{self, Selector};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{
    model::{Repository, Workspace},
    validate,
};
use shoal_store as store;
use tokio::sync::OwnedMutexGuard;

use crate::Repositories;

impl Repositories {
    /// Every registered repository, the most recently used first.
    pub async fn list(&self) -> Result<Vec<Repository>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {} FROM repositories ORDER BY last_used DESC",
                        store::REPOSITORY_COLUMNS
                    ))?
                    .query_map([], store::repository)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Resolve an ID, path, source URL, explicit name, derived name, or
    /// equivalent remote to a registered repository.
    pub async fn repository(&self, selector: impl Into<Selector>) -> Result<Repository> {
        Ok(repository::select(&self.list().await?, selector)
            .await?
            .clone())
    }

    /// Name the repository `selector` names, refusing a name another has.
    pub async fn rename(&self, selector: impl Into<Selector>, name: String) -> Result<Repository> {
        validate::name("repository", &name)?;
        let repo = self.repository(selector).await?;
        self.store
            .run(move |db| {
                ensure!(
                    !store::exists(
                        db,
                        "SELECT 1 FROM repositories WHERE name=?1 AND id<>?2",
                        params![name, repo.id]
                    )?,
                    "repository name is already in use"
                );
                db.execute(
                    "UPDATE repositories SET name=?2 WHERE id=?1",
                    params![repo.id, name],
                )?;
                Ok(db.query_row(
                    &format!(
                        "SELECT {} FROM repositories WHERE id=?1",
                        store::REPOSITORY_COLUMNS
                    ),
                    [repo.id],
                    store::repository,
                )?)
            })
            .await
    }

    /// Refuse the repository `id` while a removal or move of it is under
    /// way or unfinished.
    pub async fn ensure_available(&self, id: &RepositoryId) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                ensure!(
                    !store::repository_removing(db, &id)?,
                    "repository {id} is being removed, or its removal is incomplete; retry shoal repo rm {id} --yes"
                );
                store::ensure_repository_not_moving(db, &id)?;
                Ok(())
            })
            .await
    }

    /// The repository `selector` names with its Git gate held, once it is
    /// available.
    pub async fn lock(
        &self,
        selector: impl Into<Selector>,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let repo = self.repository(selector).await?;
        let guard = self.gates.lock_git(&repo.id).await;
        // Removal may have completed or failed while this request waited.
        let repo = self.repository(&repo.id).await?;
        self.ensure_available(&repo.id).await?;
        Ok((repo, guard))
    }

    /// Every workspace, as their records stand.
    pub(crate) async fn workspaces(&self) -> Result<Vec<Workspace>> {
        let records = self.records.clone();
        self.store.run(move |db| records.workspaces(db)).await
    }
}
