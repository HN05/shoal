//! Removal records: every removal of an existing worktree takes a checkpoint
//! and records what `shoal restore` needs to reopen the workspace, until the
//! record expires.
use crate::client::space_ids::SpaceId;
use crate::execution::recovery::LastSessionExt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::{client::space_names::SpaceName, model::SpaceKind};
use crate::{
    daemon::events::EventCause,
    git,
    model::{
        CheckpointCause, OpenedWorkspace, Relation, RemovedWorkspace, Repository,
        RestoredWorkspace, SpaceLinks, Workspace,
    },
    time::UnixTime,
    transfer::CarriedSessionFile,
};

impl Manager {
    /// Take the removal's checkpoint and record what `shoal restore` reopens
    /// before any file is deleted, so an interrupted or failed removal stays
    /// restorable. Records of workspaces that still exist are not listed.
    pub(super) async fn save_removal_record(
        &self,
        workspace: &Workspace,
        cause: EventCause,
    ) -> Result<RemovedWorkspace> {
        let checkpoint = self
            .checkpoint(workspace, CheckpointCause::Removal, None)
            .await?;
        // Removal refuses link changes, so links read now are final, unlike
        // those of the snapshot taken before it started.
        let links = self.workspace_by_id(&workspace.id).await?.links;
        // Removal has stopped every agent, and deletes the state their
        // records are in.
        let agents = crate::execution::recovery::stopped_agents(&self.paths, &workspace.id)?;
        // An unreadable last session is not worth keeping the workspace for.
        let last_sessions =
            crate::execution::recovery::LastSession::all(&self.paths, &workspace.id)
                .unwrap_or_default();
        let removed = RemovedWorkspace {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            name: workspace.name.clone(),
            expires_at: checkpoint.expires_at,
            checkpoint,
            base_ref: workspace.base_ref.clone(),
            base_commit: workspace.base_commit.clone(),
            at: workspace.at.clone(),
            links,
            agents,
            last_sessions,
            session_files: self.unplaced_sessions(workspace).await,
            cause,
            removed_at: UnixTime::now(),
        };
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// Whether a checkpoint would keep all of the workspace's unsaved files:
    /// within the archive limit, or a copy's starting files whatever their
    /// size, which its starting checkpoint already holds.
    pub(crate) async fn archives_unsaved_files(&self, workspace: &Workspace) -> Result<bool> {
        if crate::removal::holds_starting_files(workspace).await? {
            return Ok(true);
        }
        let settings = self
            .workspace_settings(workspace)
            .await?
            .into_settings()
            .cleanup
            .restore;
        Ok(git::checkpoint::unsaved_bytes(&workspace.path)
            .await?
            .is_some_and(|bytes| bytes <= settings.archive_max_bytes()))
    }

    /// Whether merged cleanup may archive the workspace's unsaved files: the
    /// branch has commits of its own, so they are left over from merged work
    /// rather than work never committed, and a checkpoint keeps them.
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
    /// its unsaved files, checkpoints, linked issue, opened and related items,
    /// and stopped agents; the caller sets it up and links its PRs. The
    /// record goes once the files are back.
    pub async fn restore_workspace(&self, selector: &SpaceSelector) -> Result<RestoredWorkspace> {
        let removed = self.claim_removed_workspace(selector).await?;
        let checkpoint = &removed.checkpoint;
        let files = (checkpoint.commit != checkpoint.head).then(|| RestoredFiles {
            head: checkpoint.head.clone(),
            snapshot: checkpoint.commit.clone(),
        });
        let opened = match removed.at.clone() {
            Some(at) => OpenedWorkspace {
                workspace: self.restore_commit_workspace(&removed, at, files).await?,
                reused: false,
            },
            None => self.restore_branch_workspace(&removed, files).await?,
        };
        let mut warnings = self.relink(&opened.workspace.id, &removed.links).await;
        let session_files = self
            .hand_over_imported_sessions(&removed, &opened.workspace.id)
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!("cannot keep the agents' session files: {error:#}"));
                Vec::new()
            });
        if let Err(error) = crate::execution::recovery::save_stopped_agents(
            &self.paths,
            &opened.workspace.id,
            &removed.agents,
        ) {
            warnings.push(format!(
                "cannot save stopped agents for shoal resume: {error:#}"
            ));
        }
        for last in &removed.last_sessions {
            if let Err(error) = last.write(&self.paths, &opened.workspace.id) {
                warnings.push(format!(
                    "cannot save the last {} session for shoal resume: {error:#}",
                    last.agent
                ));
            }
        }
        self.adopt_removal(&removed, &opened.workspace.id).await?;
        Ok(RestoredWorkspace {
            opened,
            removed,
            session_files,
            warnings,
        })
    }

    /// Open the removed workspace `removed` again on its branch at the
    /// removed HEAD, with `files` from its checkpoint.
    async fn restore_branch_workspace(
        &self,
        removed: &RemovedWorkspace,
        files: Option<RestoredFiles>,
    ) -> Result<OpenedWorkspace> {
        let branch = self.restore_branch(removed).await?;
        let base = self.restore_base(removed).await?;
        let opened = self
            .open_branch_restoring(
                &removed.repository_id,
                &branch,
                base,
                Default::default(),
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
        Ok(opened)
    }

    /// Open a workspace another Shoal exported: record it as removed here
    /// with its checkpoint, fetched from `bundle` or else on the default
    /// branch, and its agents' `session_files`, and restore it; a `copy` as
    /// a new workspace of its own. A failed restore leaves the record for
    /// `shoal restore`.
    pub async fn import_workspace(
        &self,
        selector: &crate::repository::Selector,
        bundle: Option<&Path>,
        prerequisite: Option<&str>,
        removed: RemovedWorkspace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RestoredWorkspace> {
        let repo = self.repository(selector).await?;
        ensure!(
            removed.checkpoint.workspace_id == removed.workspace_id,
            "the export's checkpoint belongs to another workspace"
        );
        if let Some(commit) = prerequisite {
            self.fetch_missing(&repo, commit).await?;
        }
        let removed = self.imported_record(&repo, removed).await?;
        let removed = {
            // A repository move holds this gate; the checkout may have moved.
            let (repo, _git) = self.lock_repository(&repo.id).await?;
            self.record_import(&repo, bundle, removed, copy, session_files)
                .await?
        };
        self.restore_workspace(&SpaceSelector::id(&removed.workspace_id))
            .await
            .with_context(|| {
                format!(
                    "the imported workspace is recorded as removed; shoal restore {} retries",
                    removed.name
                )
            })
    }

    /// Fetch the default branch when the repository lacks `commit`, which
    /// the export builds on.
    async fn fetch_missing(&self, repo: &Repository, commit: &str) -> Result<()> {
        if git::has_commit(&repo.path, commit).await? {
            return Ok(());
        }
        self.fetch_default_branch(&repo.id)
            .await
            .context("fetch the default branch the export builds on")
    }

    /// The exported record as this repository's, expiring after its
    /// retention as a removal here would.
    async fn imported_record(
        &self,
        repo: &Repository,
        removed: RemovedWorkspace,
    ) -> Result<RemovedWorkspace> {
        let retention = self
            .repository_settings(&repo.id)
            .await?
            .cleanup
            .restore
            .retention();
        let expires_at = UnixTime::now() + retention;
        let mut removed = RemovedWorkspace {
            repository_id: repo.id.clone(),
            expires_at,
            ..removed
        };
        removed.checkpoint.repository_id = repo.id.clone();
        removed.checkpoint.expires_at = expires_at;
        Ok(removed)
    }

    /// The exported `removed` as a new workspace of `repo` that copies it:
    /// with its own ID, on a branch of its own named after the original's,
    /// created at its HEAD, and without the PRs and CI runs, which stay with
    /// the original's branch. The caller holds the repository's Git gate,
    /// so a concurrent import takes another branch.
    async fn copied_record(
        &self,
        repo: &Repository,
        removed: RemovedWorkspace,
    ) -> Result<RemovedWorkspace> {
        let id = SpaceId::generate();
        let mut checkpoint = removed.checkpoint;
        checkpoint.workspace_id = id.clone();
        let name = match &checkpoint.branch {
            Some(branch) => {
                let branch = self.copy_branch(repo, branch).await?;
                git::branch::create(&repo.path, &branch, &checkpoint.head).await?;
                checkpoint.branch = Some(branch.clone());
                SpaceName::derive(SpaceKind::Workspace, &branch)
            }
            None => removed.name,
        };
        Ok(RemovedWorkspace {
            workspace_id: id,
            name,
            checkpoint,
            links: removed.links.copied(),
            ..removed
        })
    }

    /// Point the checkpoint's ref at its commit, keep the agents'
    /// `session_files`, then record the checkpoint and the removal in one
    /// transaction, as a removal would; a `copy` as the new workspace that
    /// copies it. The caller holds the repository's Git gate.
    async fn record_import(
        &self,
        repo: &Repository,
        bundle: Option<&Path>,
        removed: RemovedWorkspace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedWorkspace> {
        let _refs = self.gates.lock_checkpoints().await;
        let commit = &removed.checkpoint.commit;
        let reference = git::checkpoint::reference(commit);
        match bundle {
            Some(bundle) => git::bundle::fetch(&repo.path, bundle, &reference).await?,
            None => git::checkpoint::save(&repo.path, &reference, commit)
                .await
                .with_context(|| format!("the default branch has no commit {commit}"))?,
        }
        let removed = match copy {
            true => self.copied_record(repo, removed).await?,
            false => removed,
        };
        let removed = self
            .keep_imported_sessions(repo, removed, session_files)
            .await?;
        let record = removed.clone();
        if let Err(error) = self
            .store
            .write(move |tx| record_imported(tx, record))
            .await
        {
            let _ = self
                .discard_imported_sessions(repo, &removed.workspace_id)
                .await;
            return Err(error);
        }
        Ok(removed)
    }

    /// Link the restored workspace's issue and recorded items again; the
    /// caller links its PRs. Each failure becomes a warning.
    async fn relink(&self, workspace_id: &SpaceId, links: &SpaceLinks) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(url) = &links.issue
            && let Err(error) = self
                .set_issue(
                    &SpaceSelector::id(workspace_id),
                    url,
                    links.issue_title.clone(),
                )
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

    /// The removal record to reopen, kept with its checkpoints for at least
    /// [`RESTORE_CLAIM`] in the same transaction, so expiry cannot delete
    /// them while the restore runs.
    pub(super) async fn claim_removed_workspace(
        &self,
        selector: &SpaceSelector,
    ) -> Result<RemovedWorkspace> {
        let name = (selector.id_or_name().map(str::to_owned)).with_context(|| {
            format!("no restorable workspace {selector}; a removed workspace has no number, so name it by name or ID")
        })?;
        self.store
            .write(move |tx| claim(tx, &name))
            .await?
            .with_context(|| {
                format!("no restorable workspace {selector}; shoal restore lists them")
            })
    }

    /// The branch to open: one at the removed HEAD, or at a later commit when
    /// nothing unsaved has to be applied, created at HEAD when it was deleted.
    async fn restore_branch(&self, removed: &RemovedWorkspace) -> Result<String> {
        let (repo, _guard) = self.lock_repository(&removed.repository_id).await?;
        let checkpoint = &removed.checkpoint;
        let branch = checkpoint
            .branch
            .clone()
            .unwrap_or_else(|| removed.name.to_string());
        if let Some(workspace) = self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository_id == repo.id && workspace.branch.as_ref() == Some(&branch)
        }) {
            bail!("workspace {} already uses branch {branch}", workspace.name);
        }
        let local = git::local_ref(&branch);
        if !git::ref_exists(&repo.path, &local, git::isolated_command).await? {
            git::branch::create(&repo.path, &branch, &checkpoint.head).await?;
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
            git::is_ancestor(&repo.path, &checkpoint.head, tip, git::isolated_command).await?,
            "branch {branch} no longer contains the removed HEAD {}; restore it under another name with git branch <name> {}",
            checkpoint.head,
            checkpoint.commit
        );
        ensure!(
            tip == checkpoint.head || checkpoint.commit == checkpoint.head,
            "branch {branch} moved past the removed HEAD; open it with shoal new and apply the unsaved files with git cherry-pick --no-commit {}",
            checkpoint.commit
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
}

/// The unsaved files of a removal's checkpoint, written into the new worktree.
pub(super) struct RestoredFiles {
    pub head: String,
    pub snapshot: String,
}

/// Write a removal checkpoint's files into a new worktree still at the removed
/// HEAD, before the workspace can be used.
pub(super) async fn write_files(worktree: &Path, files: &RestoredFiles) -> Result<()> {
    let head = git::revision(worktree, "HEAD", git::run).await?;
    ensure!(
        head.trim() == files.head,
        "the branch moved during the restore; apply the unsaved files with git cherry-pick --no-commit {}",
        files.snapshot
    );
    git::checkpoint::apply(worktree, &files.snapshot).await
}

/// Persist a removal record, replacing one an earlier attempt left.
fn record(db: &Connection, removed: &RemovedWorkspace) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            removed.workspace_id,
            removed.repository_id,
            removed.name,
            removed.expires_at,
            serde_json::to_string(removed)?,
        ],
    )?;
    Ok(())
}

/// Record an imported workspace's checkpoint under its next number and its
/// removal record naming that number, unless the repository is being removed.
fn record_imported(tx: &rusqlite::Transaction<'_>, mut removed: RemovedWorkspace) -> Result<()> {
    anyhow::ensure!(
        !crate::daemon::store::repository_removing(tx, &removed.repository_id)?,
        "the repository is being removed"
    );
    removed.checkpoint.number = super::checkpoints::insert_numbered(tx, &removed.checkpoint)?;
    record(tx, &removed)?;
    Ok(())
}

/// How long a restore that started keeps its removal record and checkpoints
/// from expiring.
pub(super) const RESTORE_CLAIM: std::time::Duration = std::time::Duration::from_secs(3600);

/// Find a removal record and keep it and its checkpoints for at least
/// [`RESTORE_CLAIM`].
fn claim(tx: &rusqlite::Transaction<'_>, selector: &str) -> Result<Option<RemovedWorkspace>> {
    let Some(mut removed) = find(tx, selector)? else {
        return Ok(None);
    };
    let until = UnixTime::now() + RESTORE_CLAIM;
    for table in ["removed_workspaces", "checkpoints"] {
        tx.execute(
            &format!(
                "UPDATE {table} SET expires_at=?2, record=json_set(record, '$.expires_at', ?2)
                 WHERE workspace_id=?1 AND expires_at < ?2"
            ),
            params![removed.workspace_id, until],
        )?;
    }
    tx.execute(
        "UPDATE removed_workspaces
         SET record=json_set(record, '$.checkpoint.expires_at', ?2)
         WHERE workspace_id=?1 AND json_extract(record, '$.checkpoint.expires_at') < ?2",
        params![removed.workspace_id, until],
    )?;
    removed.expires_at = removed.expires_at.max(until);
    removed.checkpoint.expires_at = removed.checkpoint.expires_at.max(until);
    Ok(Some(removed))
}

/// The unexpired removal record of a removed workspace, by name or ID; the
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

#[cfg(test)]
pub(super) mod tests;
