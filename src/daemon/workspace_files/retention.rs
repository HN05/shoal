//! How long attached files stay: a workspace's files go with it, except its
//! results, which stay as long as its removal record, and every file goes
//! with its repository.
use anyhow::{Result, ensure};
use rusqlite::params;

use super::{FileSource, Owner, layout::Layout};
use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
};

impl Manager {
    /// Delete the files attached to `workspace` once it is removed, with
    /// their records; its results stay until [`Manager::expire_results`].
    /// Results it was the parent of move to its own parent, as its children
    /// do, so the workspaces above keep reaching them.
    pub(crate) async fn delete_workspace_files(&self, workspace: &Workspace) -> Result<()> {
        let _files = self
            .gates
            .lock_message_files(&workspace.repository_id)
            .await;
        let id = workspace.id.clone();
        let parent = workspace
            .parent_workspace
            .as_ref()
            .map(|parent| parent.id.clone());
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "DELETE FROM workspace_files WHERE workspace_id=?1 AND source!=?2",
                    params![id, FileSource::Result],
                )?;
                tx.execute(
                    "UPDATE workspace_files SET parent_id=?2 WHERE parent_id=?1",
                    params![id, parent],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        let id = workspace.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_workspace(&id)).await?
    }

    /// Delete the directory of attached files with the repository, once its
    /// workspaces are gone.
    pub(crate) async fn delete_repository_files(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        let id = repo.id.clone();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM workspace_files WHERE repository_id=?1", [id])?;
                Ok(())
            })
            .await?;
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || layout.remove()).await?
    }

    /// Delete the results of removed workspaces whose removal record has
    /// expired, or that never had one. A failure keeps what it concerns for
    /// the next pass and does not skip the rest.
    pub async fn expire_results(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if let Err(error) = self.expire_repository_results(&repo).await {
                failed.push(format!("{}: {error:#}", repo.path.display()));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_repository_results(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        let id = repo.id.clone();
        let expired: Vec<String> = self
            .store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT workspace_id FROM workspace_files WHERE repository_id=?1
                            AND workspace_id NOT IN (SELECT id FROM workspaces)
                            AND workspace_id NOT IN (SELECT workspace_id FROM removed_workspaces)",
                    )?
                    .query_map([id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        for workspace_id in expired {
            self.delete_results_of(repo, workspace_id).await?;
        }
        Ok(())
    }

    /// The removed workspaces whose results are kept, those that handed them
    /// back first oldest first.
    pub(crate) async fn kept_results(&self) -> Result<Vec<Owner>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT workspace_id,workspace,repository_id,parent_id FROM workspace_files
                            WHERE workspace_id NOT IN (SELECT id FROM workspaces)
                            GROUP BY workspace_id ORDER BY MIN(id)",
                    )?
                    .query_map([], |row| {
                        Ok(Owner {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            repository_id: row.get(2)?,
                            parent_id: row.get(3)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Delete the results that the removed workspace `owner` handed back.
    pub(crate) async fn delete_results(&self, owner: &Owner) -> Result<()> {
        let repo = self.repository(&owner.repository_id).await?;
        let _files = self.gates.lock_message_files(&repo.id).await;
        self.delete_results_of(&repo, owner.id.clone()).await
    }

    /// Delete the results of the workspace `workspace_id` of `repo`, and then
    /// their records. Callers hold the gate of the repository's files.
    async fn delete_results_of(&self, repo: &Repository, workspace_id: String) -> Result<()> {
        if let Some(layout) = Layout::of(repo) {
            let id = workspace_id.clone();
            tokio::task::spawn_blocking(move || layout.remove_results(&id)).await??;
        }
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM workspace_files WHERE workspace_id=?1",
                    [workspace_id],
                )?;
                Ok(())
            })
            .await
    }
}
