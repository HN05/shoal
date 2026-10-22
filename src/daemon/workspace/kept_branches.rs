//! Branches removal kept because only they had some commits. Cleanup deletes
//! each once a remote or the default branch has those commits too, the user
//! deletes the rest on request, and a branch that moved, went away or belongs
//! to a workspace again is forgotten.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;

use super::Manager;
use crate::{
    daemon::log,
    git::{self, default_branch::DefaultBranchLookup},
    model::{BranchTarget, KeptBranch, Repository, Workspace},
    removal::RemovalResult,
    time::unix_seconds,
};

/// A remembered branch, at the commit removal left it on.
#[derive(Debug, Clone)]
struct Record {
    repository_id: String,
    branch: String,
    head: String,
    kept_at: u64,
}

/// Where a kept branch's commits may also be.
#[derive(Debug, Clone, Copy)]
enum Evidence<'a> {
    /// The remote-tracking refs, which may be stale: enough to show a count.
    TrackingRefs,
    /// What the remotes advertise now and this repository has, for deleting
    /// without asking.
    Advertised(&'a [git::RemoteHead]),
}

/// What a kept branch is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Deleted, moved, made symbolic, the default branch or a workspace's:
    /// no longer Shoal's to delete.
    Released,
    /// Checked out by a worktree Shoal does not manage, or the start of a
    /// rebase or bisect in one.
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
        self.save_kept_branch(Record {
            repository_id: repo.id,
            branch,
            head,
            kept_at: unix_seconds(),
        })
        .await
    }

    /// Repositories with kept branches, in a stable order.
    pub async fn kept_branch_repositories(&self) -> Result<Vec<String>> {
        self.store
            .run(|db| {
                let mut statement = db.prepare(
                    "SELECT DISTINCT repository_id FROM kept_branches ORDER BY repository_id",
                )?;
                let ids = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .await
    }

    /// Delete the repository's kept branches whose commits the default branch
    /// or one of `heads`, which the remotes advertise now, has. The caller has
    /// fetched the default branch.
    pub async fn delete_kept_branches_kept_elsewhere(
        &self,
        repository_id: &str,
        heads: &[git::RemoteHead],
    ) -> Result<()> {
        let repo = self.repository(repository_id).await?;
        // Creation opens branches under the same gate.
        let _git = self.lock_repository_git(&repo.id).await;
        // An unresolved default branch could be any of them.
        let Ok(default) =
            git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
        else {
            return Ok(());
        };
        let present = git::present_heads(&repo.path, heads).await?;
        let evidence = Evidence::Advertised(&present);
        for (record, status) in self.kept_statuses(&repo, Some(&default), evidence).await? {
            if status == (Status::Kept { unkept_commits: 0 }) {
                git::branch::delete_at(&repo.path, &record.branch, &record.head).await?;
                self.forget_kept_branch(&record).await?;
                log!(
                    "deleted kept branch {}: a remote or the default branch has its commits",
                    record.branch
                );
            }
        }
        Ok(())
    }

    /// Every kept branch Shoal could delete, with the commits no
    /// remote-tracking ref or the default branch has. The count reads refs
    /// that may be stale, so a branch it shows none for is listed too.
    pub async fn list_kept_branches(&self) -> Result<Vec<KeptBranch>> {
        let mut listed = Vec::new();
        for repository_id in self.kept_branch_repositories().await? {
            let repo = self.repository(&repository_id).await?;
            let _git = self.lock_repository_git(&repo.id).await;
            let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
                .await
                .ok();
            let statuses = self
                .kept_statuses(&repo, default.as_deref(), Evidence::TrackingRefs)
                .await?;
            for (record, status) in statuses {
                if let Status::Kept { unkept_commits } = status {
                    listed.push(kept_branch(&repo, record, unkept_commits).await?);
                }
            }
        }
        Ok(listed)
    }

    /// Delete the targets with the commits only they have, checking every
    /// one before deleting any. A target must be a kept branch still at the
    /// commit removal left it on and the user saw, not checked out, in a
    /// repository whose default branch resolves.
    pub async fn delete_kept_branches(
        &self,
        targets: Vec<BranchTarget>,
    ) -> Result<Vec<KeptBranch>> {
        let mut repository_ids: Vec<_> = targets
            .iter()
            .map(|target| target.repository_id.clone())
            .collect();
        repository_ids.sort();
        repository_ids.dedup();
        // Each repository's gate stays held until its branches are gone;
        // taking them in order keeps two requests from waiting on each other.
        let mut gates = Vec::new();
        let mut chosen = Vec::new();
        for repository_id in &repository_ids {
            let repo = self.repository(repository_id).await?;
            gates.push(self.lock_repository_git(&repo.id).await);
            // An unresolved default branch could be any of them.
            let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
                .await
                .context("the default branch is unknown, and could be any kept branch")?;
            let statuses = self
                .kept_statuses(&repo, Some(&default), Evidence::TrackingRefs)
                .await?;
            for target in targets.iter().filter(|t| &t.repository_id == repository_id) {
                let (record, status) = statuses
                    .iter()
                    .find(|(record, _)| record.branch == target.branch)
                    .with_context(|| format!("{} is not a branch Shoal kept", target.branch))?;
                ensure!(
                    record.head == target.head,
                    "{} changed since it was listed; list kept branches again",
                    target.branch
                );
                let unkept_commits = match status {
                    Status::Kept { unkept_commits } => *unkept_commits,
                    Status::CheckedOut => bail!(
                        "{} is checked out; switch that checkout away before deleting it",
                        target.branch
                    ),
                    Status::Released => {
                        bail!("{} is no longer a branch Shoal kept", target.branch)
                    }
                };
                chosen.push((repo.clone(), record.clone(), unkept_commits));
            }
        }
        let mut deleted = Vec::new();
        for (repo, record, unkept_commits) in chosen {
            let branch = kept_branch(&repo, record.clone(), unkept_commits).await?;
            git::branch::delete_at(&repo.path, &record.branch, &record.head).await?;
            self.forget_kept_branch(&record).await?;
            deleted.push(branch);
        }
        Ok(deleted)
    }

    /// Every remembered branch of the repository with its status,
    /// forgetting the released ones. The caller holds the repository's Git
    /// gate; `default` is its default branch when that resolves.
    async fn kept_statuses(
        &self,
        repo: &Repository,
        default: Option<&str>,
        evidence: Evidence<'_>,
    ) -> Result<Vec<(Record, Status)>> {
        let workspaces = self.workspace_branches(&repo.id).await?;
        let mut statuses = Vec::new();
        for record in self.kept_records(&repo.id).await? {
            let status = kept_status(&repo.path, &record, default, &workspaces, evidence).await?;
            if status == Status::Released {
                self.forget_kept_branch(&record).await?;
            }
            statuses.push((record, status));
        }
        Ok(statuses)
    }

    /// Store the record, replacing an earlier one of the same branch.
    async fn save_kept_branch(&self, record: Record) -> Result<()> {
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO kept_branches(repository_id,branch,head,kept_at) VALUES (?1,?2,?3,?4)
                        ON CONFLICT(repository_id,branch) DO UPDATE
                        SET head=excluded.head, kept_at=excluded.kept_at",
                    params![
                        record.repository_id,
                        record.branch,
                        record.head,
                        record.kept_at as i64
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn kept_records(&self, repository_id: &str) -> Result<Vec<Record>> {
        let repository_id = repository_id.to_owned();
        self.store
            .run(move |db| {
                let mut statement = db.prepare(
                    "SELECT repository_id,branch,head,kept_at FROM kept_branches
                        WHERE repository_id=?1 ORDER BY kept_at, branch",
                )?;
                let records = statement
                    .query_map([repository_id], |row| {
                        Ok(Record {
                            repository_id: row.get(0)?,
                            branch: row.get(1)?,
                            head: row.get(2)?,
                            kept_at: row.get::<_, i64>(3)?.max(0) as u64,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(records)
            })
            .await
    }

    async fn forget_kept_branch(&self, record: &Record) -> Result<()> {
        let (repository_id, branch) = (record.repository_id.clone(), record.branch.clone());
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
    record: &Record,
    default: Option<&str>,
    workspaces: &[String],
    evidence: Evidence<'_>,
) -> Result<Status> {
    if default == Some(record.branch.as_str()) || workspaces.contains(&record.branch) {
        return Ok(Status::Released);
    }
    let commit = git::status::local_branch_commit(repo, &record.branch).await?;
    if commit.as_deref() != Some(record.head.as_str())
        || git::branch::is_symbolic(repo, &record.branch).await?
    {
        return Ok(Status::Released);
    }
    if git::checkout_of(repo, &record.branch, git::run)
        .await?
        .is_some()
        || git::branch::in_operation(repo, &record.branch).await?
    {
        return Ok(Status::CheckedOut);
    }
    let unkept_commits = match (default, evidence) {
        (Some(default), Evidence::Advertised(heads)) => {
            git::status::unkept_commits_by(repo, &record.head, default, heads).await?
        }
        (Some(default), Evidence::TrackingRefs) => {
            git::status::unkept_commits(repo, &record.head, default).await?
        }
        (None, _) => git::status::unretained_commits(repo, &record.head, None).await?,
    };
    Ok(Status::Kept { unkept_commits })
}

async fn kept_branch(repo: &Repository, record: Record, unkept_commits: u64) -> Result<KeptBranch> {
    let (committed_at, subject) = git::branch::commit_summary(&repo.path, &record.head).await?;
    Ok(KeptBranch {
        repository_id: record.repository_id,
        repository: repo
            .name
            .clone()
            .unwrap_or_else(|| repo.path.display().to_string()),
        branch: record.branch,
        head: record.head,
        kept_at: record.kept_at,
        unpushed_commits: unkept_commits,
        committed_at,
        subject,
    })
}

#[cfg(test)]
mod tests;
