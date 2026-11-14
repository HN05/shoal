//! Removal records: every removal of an existing worktree takes a checkpoint
//! and records what `shoal restore` needs to reopen the workspace, until the
//! record expires.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use crate::execution::recovery::LastSessionExt;
use shoal_refs::{BranchName, CommitId};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use super::commits::NewRecord;
use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::daemon::scope::Caller;
use crate::refusal::{Reason, Refusal};
use crate::{
    daemon::events::EventCause,
    git,
    model::{
        CheckpointCause, OpenedWorkspace, RemovedKind, RemovedSpace, RemovedWorkspace, Repository,
        RestoredWorkspace, Workspace,
    },
    time::UnixTime,
    transfer::CarriedSessionFile,
};

impl Manager {
    /// Take the removal's checkpoint and record what `shoal restore` reopens
    /// before any file is deleted, so an interrupted or failed removal stays
    /// restorable, leaving out the agent `carried_on` ran as, which carries on
    /// elsewhere, as in the chat a workspace becomes. Records of workspaces
    /// that still exist are not listed.
    pub(super) async fn save_removal_record(
        &self,
        workspace: &Workspace,
        cause: EventCause,
        carried_on: Option<&ExecutionId>,
    ) -> Result<RemovedSpace> {
        let checkpoint = self
            .checkpoint(workspace, CheckpointCause::Removal, None)
            .await?;
        // Removal refuses link changes, so links read now are final, unlike
        // those of the snapshot taken before it started; the spaces below go
        // first, so its parent is still there.
        let Workspace { links, parent, .. } = self.workspace_by_id(&workspace.id).await?;
        // Removal has stopped every agent, and deletes the state their
        // records are in.
        let mut agents = crate::execution::recovery::stopped_agents(&self.paths, &workspace.id)?;
        agents.retain(|agent| Some(&agent.execution) != carried_on);
        // An unreadable last session is not worth keeping the workspace for.
        let mut last_sessions =
            crate::execution::recovery::LastSession::all(&self.paths, &workspace.id)
                .unwrap_or_default();
        last_sessions.retain(|last| Some(&last.execution) != carried_on);
        let removed = RemovedSpace {
            space_id: workspace.id.clone(),
            repository_id: Some(workspace.repository_id.clone()),
            name: workspace.name.clone(),
            expires_at: checkpoint.expires_at,
            parent,
            links,
            agents,
            last_sessions,
            cause,
            removed_at: UnixTime::now(),
            kind: RemovedKind::Workspace(Box::new(RemovedWorkspace {
                checkpoint,
                base_ref: workspace.base_ref.clone(),
                base_commit: workspace.base_commit.clone(),
                at: workspace.at.clone(),
                setup_skipped: workspace.setup_skipped,
                session_files: self.unplaced_sessions(workspace).await,
            })),
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
        Ok(
            git::status::commits_since(&workspace.path, base.as_str()).await? > 0
                && self.archives_unsaved_files(workspace).await?,
        )
    }

    /// The workspaces among [`Self::removed_spaces`].
    pub(crate) async fn removed_workspaces(&self) -> Result<Vec<RemovedSpace>> {
        let mut removed = self.removed_spaces().await?;
        removed.retain(|removed| removed.workspace().is_some());
        Ok(removed)
    }

    /// Restorable spaces, newest first; expired ones are gone even before
    /// the sweep deletes them.
    pub async fn removed_spaces(&self) -> Result<Vec<RemovedSpace>> {
        self.store
            .run(|db| {
                db.prepare(
                    "SELECT record FROM removed_spaces WHERE expires_at > unixepoch()
                     AND space_id NOT IN (SELECT id FROM workspaces) ORDER BY rowid DESC",
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
        self.restore_workspace_as(selector, None).await
    }

    /// [`Self::restore_workspace`] for a scoped `caller`, which reopens it
    /// below its own space, which must take a child of the workspace's
    /// repository, as a scoped start opens one, so it stays in its tree.
    pub(crate) async fn restore_workspace_below(
        &self,
        selector: &SpaceSelector,
        caller: &Caller,
    ) -> Result<RestoredWorkspace> {
        let repository = self.restorable_below(selector, caller).await?;
        let restored = self.restore_workspace_as(selector, Some(caller)).await?;
        let space = restored.opened.workspace.id.clone();
        let parent = caller.space_id.clone();
        self.store
            .write(move |tx| {
                super::tree::ready_parent(tx, Some(&repository), &parent).with_context(|| {
                    format!("the restored workspace {space} stays at the top level")
                })?;
                super::tree::set_parent(tx, &space, Some(&parent))
            })
            .await?;
        Ok(restored)
    }

    /// The repository of the removed workspace `selector` names, once the
    /// space of `caller` takes a child of it, as a scoped restore of it
    /// places the workspace there.
    pub(crate) async fn restorable_below(
        &self,
        selector: &SpaceSelector,
        caller: &Caller,
    ) -> Result<RepositoryId> {
        let removed = (self.removed_spaces().await?.into_iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| {
                format!("no restorable workspace {selector}; shoal restore lists them")
            })?;
        let repository = removed.repository()?.clone();
        let parent = caller.space_id.clone();
        self.store
            .read(move |tx| {
                super::tree::ready_parent(tx, Some(&repository), &parent)?;
                Ok(repository)
            })
            .await
    }

    /// [`Self::restore_workspace`] for `caller`, which relinks only what it
    /// may link.
    async fn restore_workspace_as(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<RestoredWorkspace> {
        let removed = self.claim_removed_workspace(selector).await?;
        let mut warnings = Vec::new();
        let new = NewRecord {
            id: self.restored_id(&removed).await?,
            parent: self.restored_parent(&removed, &mut warnings).await?,
        };
        let kept = removed.kept_workspace()?;
        let checkpoint = &kept.checkpoint;
        let files = (checkpoint.commit != checkpoint.head).then(|| RestoredFiles {
            head: checkpoint.head.to_string().into(),
            snapshot: checkpoint.commit.to_string().into(),
        });
        let opened = match kept.at.clone() {
            Some(at) => OpenedWorkspace {
                workspace: (self.restore_commit_workspace(&removed, new, at, files)).await?,
                reused: false,
            },
            None => self.restore_branch_workspace(&removed, new, files).await?,
        };
        let id = &opened.workspace.id;
        warnings.extend(self.relink_space(id, &removed.links, caller).await);
        let session_files = self.restore_agents(&removed, id, &mut warnings).await;
        self.adopt_removal(&removed, id).await?;
        Ok(RestoredWorkspace {
            opened,
            removed,
            session_files,
            warnings,
        })
    }

    /// Save for the agents of the space `id` that restores `removed` what
    /// its record keeps of them: their stopped agents and last sessions, and
    /// the session files an import carried, which it returns for the caller
    /// to put in place. Each that fails adds a warning to `warnings`.
    async fn restore_agents(
        &self,
        removed: &RemovedSpace,
        id: &SpaceId,
        warnings: &mut Vec<String>,
    ) -> Vec<CarriedSessionFile> {
        let session_files = (self.hand_over_imported_sessions(removed, id))
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!("cannot keep the agents' session files: {error:#}"));
                Vec::new()
            });
        let saved =
            crate::execution::recovery::save_stopped_agents(&self.paths, id, &removed.agents);
        if let Err(error) = saved {
            warnings.push(format!(
                "cannot save stopped agents for shoal restore: {error:#}"
            ));
        }
        for last in &removed.last_sessions {
            if let Err(error) = last.write(&self.paths, id) {
                warnings.push(format!(
                    "cannot save the last {} session for shoal restore: {error:#}",
                    last.agent
                ));
            }
        }
        session_files
    }

    /// The space the restore of `removed` opens it below: its recorded
    /// parent while that exists and takes it, as a start places a child;
    /// otherwise none, with a warning in `warnings` naming the parent.
    async fn restored_parent(
        &self,
        removed: &RemovedSpace,
        warnings: &mut Vec<String>,
    ) -> Result<Option<SpaceId>> {
        let Some(parent) = removed.parent.clone() else {
            return Ok(None);
        };
        let (repository, space) = (removed.repository_id.clone(), removed.space_id.clone());
        let id = parent.id.clone();
        let taken = (self.store)
            .read(move |tx| {
                Ok(super::tree::ensure_parent(
                    tx,
                    repository.as_ref(),
                    &space,
                    &id,
                ))
            })
            .await?;
        match taken {
            Ok(()) => Ok(Some(parent.id)),
            Err(error) => {
                warnings.push(format!(
                    "{} opens at the top level, since its parent {} ({}) cannot take it: {error:#}",
                    removed.name, parent.name, parent.id
                ));
                Ok(None)
            }
        }
    }

    /// The ID the restore of `removed` opens it under: its own, unless the
    /// chat a conversion made of it has it, beside which it opens as a new
    /// space.
    async fn restored_id(&self, removed: &RemovedSpace) -> Result<Option<SpaceId>> {
        let taken = self.find_space(&removed.space_id).await?.is_some();
        Ok((!taken).then(|| removed.space_id.clone()))
    }

    /// Open the removed workspace `removed` again as `new` places it, on its
    /// branch at the removed HEAD, with `files` from its checkpoint.
    async fn restore_branch_workspace(
        &self,
        removed: &RemovedSpace,
        new: NewRecord,
        files: Option<RestoredFiles>,
    ) -> Result<OpenedWorkspace> {
        let branch = self.restore_branch(removed).await?;
        let base = self.restore_base(removed).await?;
        let new = super::NewWorkspace {
            options: crate::protocol::CreationOptions {
                // A name an older Shoal recorded may be longer than one set
                // now allows.
                name: Some(SpaceName::derive(removed.name.as_str(), "workspace")),
                skip_setup: removed.kept_workspace()?.setup_skipped,
                ..Default::default()
            },
            id: new.id,
            parent: new.parent,
            ..Default::default()
        };
        let opened = self
            .open_branch_restoring(removed.repository()?, branch.as_str(), base, new, files)
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
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RestoredWorkspace> {
        let removed = self
            .record_package(selector, bundle, prerequisite, removed, copy, session_files)
            .await
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        self.restore_workspace(&SpaceSelector::id(&removed.space_id))
            .await
            .with_context(|| {
                format!(
                    "the imported workspace is recorded as removed; shoal restore {} retries",
                    removed.space_id
                )
            })
    }

    /// Record the packaged `removed` as a removed workspace of the
    /// repository `selector` names, as [`Self::import_workspace`] does
    /// before opening it; an error leaves no record.
    async fn record_package(
        &self,
        selector: &crate::repository::Selector,
        bundle: Option<&Path>,
        prerequisite: Option<&str>,
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let repo = self.repository(selector).await?;
        ensure!(
            removed.kept_workspace()?.checkpoint.workspace_id == removed.space_id,
            "the export's checkpoint belongs to another workspace"
        );
        if let Some(commit) = prerequisite {
            self.fetch_missing(&repo, &commit.into()).await?;
        }
        let removed = self.imported_record(&repo, removed).await?;
        // A repository move holds this gate; the checkout may have moved.
        let (repo, _git) = self.lock_repository(&repo.id).await?;
        self.record_import(&repo, bundle, removed, copy, session_files)
            .await
    }

    /// Fetch the default branch when the repository lacks `commit`, which
    /// the export builds on.
    async fn fetch_missing(&self, repo: &Repository, commit: &CommitId) -> Result<()> {
        if git::has_commit(&repo.path, commit.as_str()).await? {
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
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let retention = self
            .repository_settings(&repo.id)
            .await?
            .cleanup
            .restore
            .retention();
        let expires_at = UnixTime::now() + retention;
        let mut removed = RemovedSpace {
            repository_id: Some(repo.id.clone()),
            expires_at,
            ..removed
        };
        let checkpoint = &mut removed.kept_workspace_mut()?.checkpoint;
        checkpoint.repository_id = repo.id.clone();
        checkpoint.expires_at = expires_at;
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
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let id = SpaceId::generate();
        let mut removed = removed;
        let checkpoint = &mut removed.kept_workspace_mut()?.checkpoint;
        checkpoint.workspace_id = id.clone();
        let name = match &checkpoint.branch {
            Some(branch) => {
                let branch = self.copy_branch(repo, branch.as_str()).await?;
                git::branch::create(&repo.path, &branch, checkpoint.head.as_str()).await?;
                checkpoint.branch = Some(branch.as_str().into());
                SpaceName::derive(branch.as_str(), "workspace")
            }
            None => removed.name.clone(),
        };
        Ok(RemovedSpace {
            space_id: id,
            name,
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
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let _refs = self.gates.lock_checkpoints().await;
        let commit = &removed.kept_workspace()?.checkpoint.commit;
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
                .discard_imported_sessions(repo, &removed.space_id)
                .await;
            return Err(error);
        }
        Ok(removed)
    }

    /// The removal record to reopen, kept with its checkpoints for at least
    /// [`RESTORE_CLAIM`] in the same transaction, so expiry cannot delete
    /// them while the restore runs.
    pub(super) async fn claim_removed_workspace(
        &self,
        selector: &SpaceSelector,
    ) -> Result<RemovedSpace> {
        let id = (selector.given_id().cloned()).with_context(|| {
            format!("no restorable workspace {selector}; a removed workspace has no number, so select it by its ID")
        })?;
        self.store
            .write(move |tx| claim(tx, &id))
            .await?
            .with_context(|| {
                format!("no restorable workspace {selector}; shoal restore lists them")
            })
    }

    /// The branch to open: one at the removed HEAD, or at a later commit when
    /// nothing unsaved has to be applied, created at HEAD when it was deleted.
    async fn restore_branch(&self, removed: &RemovedSpace) -> Result<BranchName> {
        let (repo, _guard) = self.lock_repository(removed.repository()?).await?;
        let checkpoint = &removed.kept_workspace()?.checkpoint;
        // A detached HEAD reopens on a branch named after its short commit,
        // since nothing is built from the workspace's name.
        let head = checkpoint.head.as_str();
        let branch = (checkpoint.branch.clone())
            .unwrap_or_else(|| BranchName::from(checkpoint.head.abbreviated(7)));
        if let Some(workspace) = self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository_id == repo.id && workspace.branch.as_ref() == Some(&branch)
        }) {
            bail!("workspace {} already uses branch {branch}", workspace.name);
        }
        let local = git::local_ref(&branch);
        if !git::ref_exists(&repo.path, &local, git::isolated_command).await? {
            git::branch::create(&repo.path, &branch, head).await?;
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
            git::is_ancestor(&repo.path, head, tip, git::isolated_command).await?,
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
    async fn restore_base(&self, removed: &RemovedSpace) -> Result<Option<String>> {
        let repo = self.repository(removed.repository()?).await?;
        let kept = removed.kept_workspace()?;
        if let Some(reference) = &kept.base_ref
            && git::ref_exists(&repo.path, reference, git::isolated_command).await?
        {
            return Ok(Some(reference.clone()));
        }
        let Some(commit) = &kept.base_commit else {
            return Ok(None);
        };
        let exists = git::verify_revision(
            &repo.path,
            &format!("{commit}^{{commit}}"),
            git::run_isolated,
        )
        .await
        .is_ok();
        Ok((exists.then(|| commit.clone())).map(String::from))
    }
}

/// The unsaved files of a removal's checkpoint, written into the new worktree.
pub(super) struct RestoredFiles {
    pub head: CommitId,
    pub snapshot: CommitId,
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
    git::checkpoint::apply(worktree, files.snapshot.as_str()).await
}

/// Persist a removal record, replacing one an earlier attempt left.
fn record(db: &Connection, removed: &RemovedSpace) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
            VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            removed.space_id,
            removed.repository_id,
            removed.workspace().is_none(),
            removed.name,
            removed.expires_at,
            serde_json::to_string(removed)?,
        ],
    )?;
    Ok(())
}

/// Record an imported workspace's checkpoint under its next number and its
/// removal record naming that number, unless the repository is being removed.
fn record_imported(tx: &rusqlite::Transaction<'_>, mut removed: RemovedSpace) -> Result<()> {
    anyhow::ensure!(
        !crate::daemon::store::repository_removing(tx, removed.repository()?)?,
        "the repository is being removed"
    );
    let checkpoint = &mut removed.kept_workspace_mut()?.checkpoint;
    checkpoint.number = super::checkpoints::insert_numbered(tx, checkpoint)?;
    record(tx, &removed)?;
    Ok(())
}

/// How long a restore that started keeps its removal record, checkpoints and
/// attached files from expiring.
pub(super) const RESTORE_CLAIM: std::time::Duration = std::time::Duration::from_secs(3600);

/// Find a removal record and keep it, its checkpoints and its attached files
/// for at least [`RESTORE_CLAIM`].
fn claim(tx: &rusqlite::Transaction<'_>, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    let Some(mut removed) = find(tx, id)? else {
        return Ok(None);
    };
    let until = UnixTime::now() + RESTORE_CLAIM;
    for (table, column) in [
        ("removed_spaces", "space_id"),
        ("checkpoints", "workspace_id"),
    ] {
        tx.execute(
            &format!(
                "UPDATE {table} SET expires_at=?2, record=json_set(record, '$.expires_at', ?2)
                 WHERE {column}=?1 AND expires_at < ?2"
            ),
            params![removed.space_id, until],
        )?;
    }
    tx.execute(
        "UPDATE removed_spaces
         SET record=json_set(record, '$.checkpoint.expires_at', ?2)
         WHERE space_id=?1 AND json_extract(record, '$.checkpoint.expires_at') < ?2",
        params![removed.space_id, until],
    )?;
    crate::daemon::workspace_files::claim(tx, &removed.space_id, until)?;
    removed.expires_at = removed.expires_at.max(until);
    if let Ok(kept) = removed.kept_workspace_mut() {
        kept.checkpoint.expires_at = kept.checkpoint.expires_at.max(until);
    }
    Ok(Some(removed))
}

/// The unexpired removal record of the removed workspace `id`.
fn find(db: &Connection, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM removed_spaces WHERE space_id=?1
                AND expires_at > unixepoch() AND space_id NOT IN (SELECT id FROM workspaces)
                ORDER BY rowid DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

#[cfg(test)]
pub(super) mod tests;
