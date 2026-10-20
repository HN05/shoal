//! Restore points for removed workspaces: every removal of an existing
//! worktree keeps its HEAD and unsaved files reachable under a private ref
//! and records what `shoal restore` needs, until the record expires.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{
    daemon::{events::EventCause, log},
    forge::link::Relation,
    git,
    model::{RemovedWorkspace, Repository, RestoredWorkspace, Workspace, WorkspaceLinks},
    removal::RemovalCheck,
    time::unix_seconds,
};

impl Manager {
    /// Keep HEAD and, within the archive limit, the unsaved files reachable,
    /// and record them before any file is deleted, so an interrupted or failed
    /// removal stays restorable. Records of workspaces that still exist are
    /// not listed.
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
        // A failed attempt may have deleted files its snapshot alone keeps.
        let earlier = self.earlier_snapshot(&workspace.id).await?;
        let message = format!("Shoal restore point for {}", workspace.name);
        let snapshot =
            git::restore::snapshot(path, &message, !files_omitted, earlier.as_deref()).await?;
        let reference = git::restore::reference(&workspace.id);
        git::restore::save(&repo.path, &reference, &snapshot).await?;
        // Removal refuses link changes, so links read now are final, unlike
        // those of the snapshot taken before it started.
        let links = self.workspace(&workspace.id).await?.links;
        // Removal has stopped every agent, and deletes the state their
        // records are in.
        let agents = crate::execution::recovery::stopped_agents(&self.paths, &workspace.id)?;
        let removed_at = unix_seconds();
        let removed = RemovedWorkspace {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            name: workspace.name.clone(),
            branch: check.branch.clone(),
            head,
            snapshot,
            files_omitted,
            base_ref: workspace.base_ref.clone(),
            base_commit: workspace.base_commit.clone(),
            links,
            agents,
            cause,
            removed_at,
            expires_at: removed_at + retention.as_secs(),
        };
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// The snapshot an earlier removal attempt of this workspace saved.
    async fn earlier_snapshot(&self, workspace_id: &str) -> Result<Option<String>> {
        let id = workspace_id.to_owned();
        let earlier = self.store.run(move |db| recorded(db, &id)).await?;
        Ok(earlier.map(|removed| removed.snapshot))
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

    /// Restorable workspaces, newest first; expired ones are gone even before
    /// the sweep deletes them.
    pub async fn removed_workspaces(&self) -> Result<Vec<RemovedWorkspace>> {
        self.store
            .run(|db| {
                db.prepare(
                    "SELECT record FROM removed_workspaces WHERE expires_at > unixepoch()
                     AND workspace_id NOT IN (SELECT id FROM workspaces) ORDER BY rowid DESC",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|record| Ok(serde_json::from_str(&record?)?))
                .collect()
            })
            .await
    }

    /// Open a removed workspace again on its branch at the removed HEAD, with
    /// its unsaved files, linked issue, opened and related items, and stopped
    /// agents; the caller sets it up and links its PRs. The record goes once
    /// the files are back.
    pub async fn restore_workspace(&self, selector: &str) -> Result<RestoredWorkspace> {
        let removed = self.removed_workspace(selector).await?;
        let branch = self.restore_branch(&removed).await?;
        let files = (removed.snapshot != removed.head).then(|| RestoredFiles {
            head: removed.head.clone(),
            snapshot: removed.snapshot.clone(),
        });
        let base = self.restore_base(&removed).await?;
        let opened = self
            .open_branch_with(
                removed.repository_id.as_str(),
                &branch,
                None,
                None,
                base,
                files,
            )
            .await?;
        // The branch check released the repository gate, so another request
        // may have opened the branch meanwhile; that workspace got no files.
        ensure!(
            !opened.reused,
            "workspace {} opened branch {branch} during the restore",
            opened.workspace.name
        );
        let mut warnings = self.relink(&opened.workspace.id, &removed.links).await;
        if let Err(error) = crate::execution::recovery::save_stopped_agents(
            &self.paths,
            &opened.workspace.id,
            &removed.agents,
        ) {
            warnings.push(format!(
                "cannot save stopped agents for shoal resume: {error:#}"
            ));
        }
        self.forget_restore_point(&removed).await?;
        Ok(RestoredWorkspace {
            opened,
            removed,
            warnings,
        })
    }

    /// Link the restored workspace's issue and recorded items again; the
    /// caller links its PRs. Each failure becomes a warning.
    async fn relink(&self, workspace_id: &str, links: &WorkspaceLinks) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(url) = &links.issue
            && let Err(error) = self
                .set_issue(workspace_id, url, links.issue_title.clone())
                .await
        {
            warnings.push(format!("cannot link issue {url}: {error:#}"));
        }
        let recorded = links
            .opened
            .iter()
            .map(|item| (Relation::Opened, item.clone()))
            .chain(
                links
                    .related
                    .iter()
                    .map(|item| (Relation::Related, item.clone())),
            )
            .collect();
        if let Err(error) = self.record_items(workspace_id, recorded).await {
            warnings.push(format!("cannot link opened and related items: {error:#}"));
        }
        warnings
    }

    async fn removed_workspace(&self, selector: &str) -> Result<RemovedWorkspace> {
        let name = selector.to_owned();
        self.store
            .run(move |db| find(db, &name))
            .await?
            .with_context(|| {
                format!("no restorable workspace {selector}; shoal restore lists them")
            })
    }

    /// The branch to open: one at the removed HEAD, or at a later commit when
    /// nothing unsaved has to be applied, created at HEAD when it was deleted.
    async fn restore_branch(&self, removed: &RemovedWorkspace) -> Result<String> {
        let (repo, _guard) = self.lock_repository(removed.repository_id.as_str()).await?;
        let branch = removed
            .branch
            .clone()
            .unwrap_or_else(|| removed.name.clone());
        if let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|workspace| workspace.repository_id == repo.id && workspace.branch == branch)
        {
            bail!("workspace {} already uses branch {branch}", workspace.name);
        }
        let local = git::local_ref(&branch);
        if !git::ref_exists(&repo.path, &local, git::isolated_command).await? {
            git::branch::create(&repo.path, &branch, &removed.head).await?;
            return Ok(branch);
        }
        let tip = git::verify_revision(
            &repo.path,
            &format!("{local}^{{commit}}"),
            git::run_isolated,
        )
        .await?;
        let tip = tip.trim();
        ensure!(
            git::is_ancestor(&repo.path, &removed.head, tip, git::isolated_command).await?,
            "branch {branch} no longer contains the removed HEAD {}; restore it under another name with git branch <name> {}",
            removed.head,
            removed.snapshot
        );
        ensure!(
            tip == removed.head || removed.snapshot == removed.head,
            "branch {branch} moved past the removed HEAD; open it with shoal new and apply the unsaved files with git cherry-pick --no-commit {}",
            removed.snapshot
        );
        Ok(branch)
    }

    /// The recorded base, so diffs and the stack link come back: its ref
    /// while it exists, otherwise its commit; `None` when neither remains.
    async fn restore_base(&self, removed: &RemovedWorkspace) -> Result<Option<String>> {
        let repo = self.repository(&removed.repository_id).await?;
        if let Some(reference) = &removed.base_ref
            && git::ref_exists(&repo.path, reference, git::isolated_command).await?
        {
            return Ok(Some(reference.clone()));
        }
        let Some(commit) = &removed.base_commit else {
            return Ok(None);
        };
        let exists = git::verify_revision(
            &repo.path,
            &format!("{commit}^{{commit}}"),
            git::run_isolated,
        )
        .await
        .is_ok();
        Ok(exists.then(|| commit.clone()))
    }

    /// Delete a restored workspace's record and the ref that kept its commits.
    async fn forget_restore_point(&self, removed: &RemovedWorkspace) -> Result<()> {
        let repo = self.repository(&removed.repository_id).await?;
        git::restore::delete(&repo.path, &git::restore::reference(&removed.workspace_id)).await?;
        let id = removed.workspace_id.clone();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
                Ok(())
            })
            .await
    }

    /// Drop a restore point and its record.
    pub(super) async fn discard_restore_point(&self, repo: &Repository, workspace_id: &str) {
        let reference = git::restore::reference(workspace_id);
        if let Err(error) = git::restore::delete(&repo.path, &reference).await {
            log!("restore point {reference} not deleted: {error:#}");
        }
        let id = workspace_id.to_owned();
        let deleted = self
            .store
            .run(move |db| {
                db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
                Ok(())
            })
            .await;
        if let Err(error) = deleted {
            log!("restore record of {workspace_id} not deleted: {error:#}");
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
        let expired: Vec<RemovedWorkspace> = self
            .store
            .run(move |db| {
                db.prepare("SELECT record FROM removed_workspaces WHERE expires_at <= ?1")?
                    .query_map([now], |row| row.get::<_, String>(0))?
                    .map(|record| Ok(serde_json::from_str(&record?)?))
                    .collect()
            })
            .await?;
        for removed in expired {
            self.expire(&removed).await?;
        }
        Ok(())
    }

    /// Delete an expired restore point unless a retried removal replaced it
    /// since it was selected: the ref goes only while it names this snapshot,
    /// and the record only while it is this one.
    async fn expire(&self, removed: &RemovedWorkspace) -> Result<()> {
        let repo = self.repository(&removed.repository_id).await?;
        let reference = git::restore::reference(&removed.workspace_id);
        if !git::restore::delete_at(&repo.path, &reference, &removed.snapshot).await? {
            return Ok(());
        }
        let (id, snapshot) = (removed.workspace_id.clone(), removed.snapshot.clone());
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM removed_workspaces
                     WHERE workspace_id=?1 AND json_extract(record,'$.snapshot')=?2",
                    [id, snapshot],
                )?;
                Ok(())
            })
            .await
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

/// The unsaved files of a restore point, written into the new worktree.
pub(super) struct RestoredFiles {
    pub head: String,
    pub snapshot: String,
}

/// Write a restore point's files into a new worktree still at the removed
/// HEAD, before the workspace can be used.
pub(super) async fn write_files(worktree: &Path, files: &RestoredFiles) -> Result<()> {
    let head = git::revision(worktree, "HEAD", git::run).await?;
    ensure!(
        head.trim() == files.head,
        "the branch moved during the restore; apply the unsaved files with git cherry-pick --no-commit {}",
        files.snapshot
    );
    git::restore::apply(worktree, &files.snapshot).await
}

/// A removal notification that says how to undo it.
pub(crate) fn removal_notice(workspace: &str, removal: &str) -> String {
    format!("{removal}; restore it with shoal restore {workspace}")
}

/// Persist a restore record, replacing one an earlier attempt left.
fn record(db: &Connection, removed: &RemovedWorkspace) -> Result<()> {
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

/// The unexpired restore record of a removed workspace, by name or ID; the
/// newest wins when one name was removed more than once.
fn find(db: &Connection, selector: &str) -> Result<Option<RemovedWorkspace>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM removed_workspaces WHERE (workspace_id=?1 OR name=?1)
                AND expires_at > unixepoch() AND workspace_id NOT IN (SELECT id FROM workspaces)
                ORDER BY rowid DESC LIMIT 1",
            [selector],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// The restore record kept for a workspace ID, whether or not it still exists.
fn recorded(db: &Connection, workspace_id: &str) -> Result<Option<RemovedWorkspace>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM removed_workspaces WHERE workspace_id=?1",
            [workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

#[cfg(test)]
mod tests;
