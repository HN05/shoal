//! Restore points for removed workspaces: every removal of an existing
//! worktree keeps its HEAD and unsaved files reachable under a private ref
//! and records what `shoal restore` needs, until the record expires.
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};

use super::Manager;
use crate::{
    daemon::{events::EventCause, log},
    git,
    model::{RemovedWorkspace, Repository, Workspace},
    removal::RemovalCheck,
    time::unix_seconds,
};

impl Manager {
    /// Keep HEAD and, within the archive limit, the unsaved files reachable
    /// before the worktree is removed. The record commits with the removal.
    pub(super) async fn save_restore_point(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        check: &RemovalCheck,
        cause: EventCause,
    ) -> Result<RemovedWorkspace> {
        let retention = self
            .workspace_settings(workspace)
            .await?
            .cleanup
            .restore
            .retention();
        let path = &workspace.path;
        let head = git::revision(path, "HEAD", git::run)
            .await?
            .trim()
            .to_owned();
        let files_omitted = check.dirty && !self.archives_unsaved_files(workspace).await?;
        let snapshot = if files_omitted {
            head.clone()
        } else {
            let message = format!("Shoal restore point for {}", workspace.name);
            git::restore::snapshot(path, &message).await?
        };
        let reference = git::restore::reference(&workspace.id);
        git::restore::save(&repo.path, &reference, &snapshot).await?;
        let removed_at = unix_seconds();
        Ok(RemovedWorkspace {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            name: workspace.name.clone(),
            branch: check.branch.clone(),
            head,
            snapshot,
            files_omitted,
            base_ref: workspace.base_ref.clone(),
            base_commit: workspace.base_commit.clone(),
            links: workspace.links.clone(),
            cause,
            removed_at,
            expires_at: removed_at + retention.as_secs(),
        })
    }

    /// Whether a restore point would keep all of the workspace's unsaved files.
    pub(super) async fn archives_unsaved_files(&self, workspace: &Workspace) -> Result<bool> {
        let settings = self.workspace_settings(workspace).await?.cleanup.restore;
        Ok(git::restore::unsaved_bytes(&workspace.path)
            .await?
            .is_some_and(|bytes| bytes <= settings.archive_max_bytes()))
    }

    /// Whether merged cleanup may archive the workspace's unsaved files: the
    /// branch has commits of its own, so they are left over from merged work
    /// rather than work never committed, and they fit the restore archive.
    pub(super) async fn unsaved_files_are_leftovers(&self, workspace: &Workspace) -> Result<bool> {
        let Some(base) = &workspace.base_commit else {
            return Ok(false);
        };
        Ok(git::status::commits_since(&workspace.path, base).await? > 0
            && self.archives_unsaved_files(workspace).await?)
    }

    /// Drop the restore point of a removal that did not happen.
    pub(super) async fn discard_restore_point(&self, repo: &Repository, workspace_id: &str) {
        let reference = git::restore::reference(workspace_id);
        if let Err(error) = git::restore::delete(&repo.path, &reference).await {
            log!("restore point {reference} not deleted: {error:#}");
        }
    }

    /// Delete expired restore records and their refs, and refs that no
    /// record or workspace owns. A failure keeps the record for the next pass
    /// and does not skip the others.
    pub async fn expire_restore_points(&self) -> Result<()> {
        let mut failed = Vec::new();
        if let Err(error) = self.delete_expired_records().await {
            failed.push(format!("{error:#}"));
        }
        for repo in self.repositories().await? {
            if let Err(error) = self.delete_unowned_refs(&repo).await {
                failed.push(format!("{}: {error:#}", repo.path.display()));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn delete_expired_records(&self) -> Result<()> {
        let now = unix_seconds() as i64;
        let expired: Vec<(String, String)> = self
            .store
            .run(move |db| {
                db.prepare(
                    "SELECT workspace_id, repository_id FROM removed_workspaces WHERE expires_at <= ?1",
                )?
                .query_map([now], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()
                .map_err(Into::into)
            })
            .await?;
        for (workspace_id, repository_id) in expired {
            let repo = self.repository(&repository_id).await?;
            git::restore::delete(&repo.path, &git::restore::reference(&workspace_id)).await?;
            self.store
                .run(move |db| {
                    db.execute(
                        "DELETE FROM removed_workspaces WHERE workspace_id=?1",
                        [workspace_id],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    /// A ref without a record is left by a removal that stopped between them.
    async fn delete_unowned_refs(&self, repo: &Repository) -> Result<()> {
        for workspace_id in git::restore::workspace_ids(&repo.path).await? {
            let id = workspace_id.clone();
            let owned = self
                .store
                .run(move |db| {
                    crate::daemon::store::exists(
                        db,
                        "SELECT 1 FROM removed_workspaces WHERE workspace_id=?1
                         UNION SELECT 1 FROM workspaces WHERE id=?1",
                        [id],
                    )
                })
                .await?;
            if !owned {
                self.discard_restore_point(repo, &workspace_id).await;
            }
        }
        Ok(())
    }
}

/// Persist a restore record in the removal's transaction.
pub(super) fn record(db: &Connection, removed: &RemovedWorkspace) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            removed.workspace_id,
            removed.repository_id,
            removed.name,
            removed.expires_at as i64,
            serde_json::to_string(removed)?,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
