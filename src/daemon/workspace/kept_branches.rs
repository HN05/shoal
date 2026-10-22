//! Branches removal kept because only they had some commits. Cleanup deletes
//! each once a remote or the default branch has those commits too, and
//! forgets one that moved, went away or belongs to a workspace again.
use std::path::Path;

use anyhow::Result;
use rusqlite::params;

use super::Manager;
use crate::{
    daemon::log,
    git::{self, default_branch::DefaultBranchLookup},
    model::Workspace,
    removal::RemovalResult,
    time::unix_seconds,
};

/// A branch removal kept, at the commit removal left it on.
#[derive(Debug, Clone)]
pub(crate) struct KeptBranch {
    pub repository_id: String,
    pub branch: String,
    pub head: String,
}

/// What a kept branch is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Deleted, moved, the default branch or a workspace's: no longer
    /// Shoal's to delete.
    Released,
    /// Checked out by a worktree Shoal does not manage.
    CheckedOut,
    /// Still at the kept commit, with this many commits only it has.
    Kept { unkept_commits: u64 },
}

impl Manager {
    /// Remember a branch the removal of `workspace` kept, unless it is the
    /// default branch, which Shoal never deletes.
    pub(super) async fn remember_kept_branch(
        &self,
        workspace: &Workspace,
        outcome: &RemovalResult,
    ) -> Result<()> {
        let Some(branch) = outcome
            .branch
            .clone()
            .filter(|_| !outcome.branch_outcome.is_deleted())
        else {
            return Ok(());
        };
        let repo = self.repository(&workspace.repository_id).await?;
        let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
            .await
            .ok();
        if default.as_deref() == Some(branch.as_str()) {
            return Ok(());
        }
        let Some(head) = git::status::local_branch_commit(&repo.path, &branch).await? else {
            return Ok(());
        };
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO kept_branches(repository_id,branch,head,kept_at) VALUES (?1,?2,?3,?4)
                        ON CONFLICT(repository_id,branch) DO UPDATE
                        SET head=excluded.head, kept_at=excluded.kept_at",
                    params![repo.id, branch, head, unix_seconds() as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Repositories with kept branches.
    pub async fn kept_branch_repositories(&self) -> Result<Vec<String>> {
        self.store
            .run(|db| {
                let mut statement =
                    db.prepare("SELECT DISTINCT repository_id FROM kept_branches")?;
                let ids = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .await
    }

    /// Delete the repository's kept branches whose commits a remote or the
    /// default branch now has, and forget those no longer Shoal's. The
    /// caller has fetched the remotes those refs come from.
    pub async fn delete_kept_branches_kept_elsewhere(&self, repository_id: &str) -> Result<()> {
        let repo = self.repository(repository_id).await?;
        // Creation opens branches under the same gate.
        let _git = self.lock_repository_git(&repo.id).await;
        // An unresolved default branch could be any of them.
        let Ok(default) =
            git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
        else {
            return Ok(());
        };
        let workspaces = self.workspace_branches(repository_id).await?;
        for kept in self.kept_branches(repository_id).await? {
            match kept_status(&repo.path, &kept, &default, &workspaces).await? {
                Status::Released => self.forget_kept_branch(&kept).await?,
                Status::Kept { unkept_commits: 0 } => {
                    git::branch::delete_at(&repo.path, &kept.branch, &kept.head).await?;
                    self.forget_kept_branch(&kept).await?;
                    log!(
                        "deleted kept branch {}: a remote or the default branch has its commits",
                        kept.branch
                    );
                }
                Status::CheckedOut | Status::Kept { .. } => {}
            }
        }
        Ok(())
    }

    async fn kept_branches(&self, repository_id: &str) -> Result<Vec<KeptBranch>> {
        let repository_id = repository_id.to_owned();
        self.store
            .run(move |db| {
                let mut statement = db.prepare(
                    "SELECT repository_id,branch,head FROM kept_branches
                        WHERE repository_id=?1 ORDER BY kept_at, branch",
                )?;
                let kept = statement
                    .query_map([repository_id], |row| {
                        Ok(KeptBranch {
                            repository_id: row.get(0)?,
                            branch: row.get(1)?,
                            head: row.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(kept)
            })
            .await
    }

    async fn forget_kept_branch(&self, kept: &KeptBranch) -> Result<()> {
        let (repository_id, branch) = (kept.repository_id.clone(), kept.branch.clone());
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM kept_branches WHERE repository_id=?1 AND branch=?2",
                    params![repository_id, branch],
                )?;
                Ok(())
            })
            .await
    }

    /// The branches of the repository's workspaces.
    async fn workspace_branches(&self, repository_id: &str) -> Result<Vec<String>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| workspace.repository_id == repository_id)
            .map(|workspace| workspace.branch)
            .collect())
    }
}

async fn kept_status(
    repo: &Path,
    kept: &KeptBranch,
    default: &str,
    workspaces: &[String],
) -> Result<Status> {
    if kept.branch == default || workspaces.contains(&kept.branch) {
        return Ok(Status::Released);
    }
    let commit = git::status::local_branch_commit(repo, &kept.branch).await?;
    if commit.as_deref() != Some(kept.head.as_str()) {
        return Ok(Status::Released);
    }
    if git::checkout_of(repo, &kept.branch, git::run)
        .await?
        .is_some()
    {
        return Ok(Status::CheckedOut);
    }
    let unkept_commits = git::status::unkept_commits(repo, &kept.head, default).await?;
    Ok(Status::Kept { unkept_commits })
}

#[cfg(test)]
mod tests;
