//! Deleting the temporary directories of workspaces and chats once they are
//! gone.
use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{
    daemon::{log, workspace::Manager},
    model::Repository,
    tmp_dirs::Layout,
};

impl Manager {
    /// Delete the temporary directory of the workspace or chat `id` of the
    /// repository `repository_id` once its record is gone.
    pub(crate) async fn delete_tmp_dir(&self, repository_id: &str, id: &str) -> Result<()> {
        let repo = self.repository(repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || layout.remove_one(&id)).await?
    }

    /// Delete every temporary directory that belongs to no workspace or chat
    /// of its repository, such as one a process created while its workspace
    /// was removed. A failure in one repository does not skip the others.
    pub(crate) async fn sweep_tmp_dirs(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if let Err(error) = self.sweep_repository_tmp_dirs(&repo).await {
                failed.push(format!("{}: {error:#}", crate::repository::name(&repo)));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn sweep_repository_tmp_dirs(&self, repo: &Repository) -> Result<()> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        // Listed before the records are read, since a directory is only
        // created once its record exists.
        let listed = {
            let layout = layout.clone();
            tokio::task::spawn_blocking(move || layout.list()).await??
        };
        if listed.is_empty() {
            return Ok(());
        }
        let repository_id = repo.id.clone();
        let owners = self
            .store
            .run(move |db| recorded_ids(db, &repository_id))
            .await?;
        let orphans: Vec<_> = listed
            .into_iter()
            .filter(|id| !owners.contains(id))
            .collect();
        for id in &orphans {
            log!("deleting the orphaned temporary directory {id}");
        }
        tokio::task::spawn_blocking(move || orphans.iter().try_for_each(|id| layout.remove_one(id)))
            .await?
    }
}

/// The IDs of the repository's workspaces and chats.
fn recorded_ids(db: &mut Connection, repository_id: &str) -> Result<BTreeSet<String>> {
    Ok(db
        .prepare(
            "SELECT id FROM workspaces WHERE repository_id=?1
             UNION SELECT id FROM chats WHERE repository_id=?1",
        )?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
