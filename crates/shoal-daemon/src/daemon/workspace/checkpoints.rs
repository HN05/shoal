//! Checkpoints: a workspace's HEAD and unsaved files, kept under private refs
//! until they expire, which `shoal restore` returns the workspace to.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::collections::HashSet;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_refs::BranchName;

use super::super::manager::Manager;
use crate::{
    daemon::{log, scope::Caller, workspace_files::HomeKey},
    git,
    model::{Checkpoint, CheckpointCause, RemovedSpace, Repository, RestoredCheckpoint, Workspace},
    state::WorkspaceState,
    time::UnixTime,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// `shoal checkpoint`: keep the workspace's current state.
    pub async fn take_checkpoint(
        &self,
        selector: &SpaceSelector,
        name: Option<String>,
    ) -> Result<Checkpoint> {
        let workspace = self.workspace(selector).await?;
        // A repository move holds this gate; the workspace may have moved.
        let (_repo, _git) = self.lock_repository(workspace.repository()).await?;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        ensure!(
            matches!(
                workspace.state,
                WorkspaceState::Ready | WorkspaceState::Stopping | WorkspaceState::Failed
            ),
            "workspace is {}; take a checkpoint once that operation finishes",
            workspace.state
        );
        if let Some(name) = &name {
            ensure!(
                name.chars().count() <= NAME_LIMIT,
                "checkpoint names have at most {NAME_LIMIT} characters"
            );
        }
        self.verify_worktree(&workspace).await?;
        self.checkpoint(&workspace, CheckpointCause::Manual, name)
            .await
    }

    /// Keep the state an agent left when it exits or is stopped. A workspace
    /// in another lifecycle operation or removed meanwhile takes none, and a
    /// failure is only logged.
    pub(crate) async fn checkpoint_after_agent(&self, workspace_id: &SpaceId) {
        let Ok(workspace) = self.workspace_by_id(workspace_id).await else {
            return;
        };
        // A repository move holds this gate; the workspace may have moved.
        let Ok((_repo, _git)) = self.lock_repository(workspace.repository()).await else {
            return;
        };
        let Ok(workspace) = self.workspace_by_id(workspace_id).await else {
            return;
        };
        if !matches!(
            workspace.state,
            WorkspaceState::Ready | WorkspaceState::Stopping
        ) {
            return;
        }
        let taken = async {
            self.verify_worktree(&workspace).await?;
            self.checkpoint(&workspace, CheckpointCause::AgentStopped, None)
                .await
        }
        .await;
        if let Err(error) = taken {
            log!("checkpoint of {} not taken: {error:#}", workspace.name);
        }
    }

    /// Keep HEAD and the unsaved files, within the archive limit where the
    /// cause is capped, reachable under a new checkpoint. Other than a manual checkpoint, one that would
    /// keep what the latest already keeps extends that one instead.
    pub(in crate::daemon) async fn checkpoint(
        &self,
        workspace: &Workspace,
        cause: CheckpointCause,
        name: Option<String>,
    ) -> Result<Checkpoint> {
        let next = self.snapshot(workspace, cause, name).await?;
        if cause != CheckpointCause::Manual
            && let Some(latest) = self.unchanged_checkpoint(workspace, &next).await?
        {
            return Ok(latest);
        }
        self.save_checkpoint(next).await
    }

    /// The workspace's current state as an unnumbered checkpoint, its files
    /// already kept in a commit.
    async fn snapshot(
        &self,
        workspace: &Workspace,
        cause: CheckpointCause,
        name: Option<String>,
    ) -> Result<Checkpoint> {
        let retention = self
            .workspace_settings(workspace)
            .await?
            .cleanup
            .restore
            .retention();
        // A copy's starting checkpoint keeps what it started with; its
        // source had nothing a checkpoint cannot hold.
        let files_omitted = cause != CheckpointCause::Start
            && !self
                .checkpoint_keeps_unsaved_files(workspace, cause)
                .await?;
        let message = format!("Shoal checkpoint of {}", workspace.name);
        let snapshot = self
            .snapshot_files(workspace, &message, !files_omitted)
            .await?;
        let created_at = UnixTime::now();
        Ok(Checkpoint {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository().clone(),
            number: CheckpointNumber::default(),
            name,
            cause,
            branch: snapshot.branch,
            head: snapshot.head,
            commit: snapshot.commit,
            files_omitted,
            created_at,
            expires_at: created_at + retention,
        })
    }

    /// A commit with HEAD and, with `files`, the unsaved files of
    /// `workspace`: those of the worktree Git knows, or those left of one it
    /// disowned laid over its branch's commit.
    async fn snapshot_files(
        &self,
        workspace: &Workspace,
        message: &str,
        files: bool,
    ) -> Result<git::checkpoint::Snapshot> {
        let Some(left) = self.left_files(workspace).await? else {
            return git::checkpoint::snapshot(&workspace.path, message, files).await;
        };
        let commit = match files {
            true => left.snapshot(message).await?,
            false => left.head().clone(),
        };
        Ok(git::checkpoint::Snapshot {
            branch: workspace.branch().cloned(),
            head: left.head().clone(),
            commit,
        })
    }

    /// Whether `workspace` still has the branch, HEAD and unsaved files
    /// `checkpoint` keeps.
    pub(crate) async fn unchanged_since(
        &self,
        workspace: &Workspace,
        checkpoint: &Checkpoint,
    ) -> Result<bool> {
        // A repository move holds this gate; the workspace may have moved.
        let (repo, _git) = self.lock_repository(workspace.repository()).await?;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        let now = self
            .snapshot(&workspace, CheckpointCause::Manual, None)
            .await?;
        keeps_same(&repo.path, checkpoint, &now).await
    }

    /// Point the commit's ref at it, then record the snapshot under the
    /// workspace's next number, so a record never outlives a missing ref; a
    /// ref left by a failed record goes with the next expiry pass, which the
    /// checkpoint mutex keeps from running in between.
    async fn save_checkpoint(&self, mut checkpoint: Checkpoint) -> Result<Checkpoint> {
        let repo = self.repository(&checkpoint.repository_id).await?;
        let _refs = self.gates.lock_checkpoints().await;
        let reference = git::checkpoint::reference(&checkpoint.commit);
        git::checkpoint::save(&repo.path, &reference, &checkpoint.commit).await?;
        let record = checkpoint.clone();
        checkpoint.number = self
            .store
            .write(move |tx| insert_numbered(tx, &record))
            .await?;
        Ok(checkpoint)
    }

    /// The latest checkpoint when it keeps what `next` would, with its expiry
    /// moved to that of `next`.
    async fn unchanged_checkpoint(
        &self,
        workspace: &Workspace,
        next: &Checkpoint,
    ) -> Result<Option<Checkpoint>> {
        let id = workspace.id.clone();
        let Some(mut latest) = self.store.run(move |db| latest(db, &id)).await? else {
            return Ok(None);
        };
        let repo = self.repository(workspace.repository()).await?;
        if !keeps_same(&repo.path, &latest, next).await? {
            return Ok(None);
        }
        latest.expires_at = latest.expires_at.max(next.expires_at);
        let record = latest.clone();
        let extended = self.store.run(move |db| extend(db, &record)).await?;
        Ok(extended.then_some(latest))
    }

    /// A page of the unexpired checkpoints of an existing workspace numbered
    /// after `after`, oldest first, small enough for one protocol frame;
    /// `None` when no workspace has that name or ID.
    pub async fn checkpoints(
        &self,
        selector: &SpaceSelector,
        after: Option<CheckpointNumber>,
    ) -> Result<Option<Vec<Checkpoint>>> {
        let Ok(workspace) = self.workspace(selector).await else {
            return Ok(None);
        };
        let id = workspace.id;
        self.store
            .run(move |db| checkpoint_page(db, &id, after.unwrap_or_default()))
            .await
            .map(Some)
    }

    /// Checkpoint `number` of `workspace`, refusing one it lacks.
    pub(crate) async fn numbered_checkpoint(
        &self,
        workspace: &Workspace,
        number: CheckpointNumber,
    ) -> Result<Checkpoint> {
        let (id, name) = (workspace.id.clone(), workspace.name.clone());
        let space = workspace.number;
        self.store
            .run(move |db| find(db, &id, number))
            .await?
            .with_context(|| {
                format!(
                    "workspace {name} has no checkpoint {number}; shoal restore {space} lists them"
                )
            })
    }

    /// Return the workspace to a checkpoint after keeping its current state
    /// in another. Nothing else may run in it meanwhile, except the caller's
    /// own execution.
    pub async fn restore_checkpoint(
        &self,
        selector: &SpaceSelector,
        number: CheckpointNumber,
        caller: Option<&Caller>,
    ) -> Result<RestoredCheckpoint> {
        let workspace = self.workspace(selector).await?;
        let checkpoint = self.numbered_checkpoint(&workspace, number).await?;
        let reserved_state = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Restoring)
            .await?;
        let result = self
            .rewind(
                &workspace,
                &checkpoint,
                caller.and_then(Caller::execution_id),
            )
            .await;
        self.set_state(
            &workspace.id,
            reserved_state,
            result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .or(workspace.error.clone()),
        )
        .await?;
        let replaced = result?;
        self.touch(&workspace.id).await;
        Ok(RestoredCheckpoint {
            workspace: self.workspace_by_id(&workspace.id).await?,
            checkpoint,
            replaced,
        })
    }

    /// Keep the current state and move the worktree to `checkpoint`, unless
    /// that checkpoint could not keep the unsaved files.
    async fn rewind(
        &self,
        workspace: &Workspace,
        checkpoint: &Checkpoint,
        own_execution: Option<&ExecutionId>,
    ) -> Result<Checkpoint> {
        self.ensure_rewindable(workspace, checkpoint, own_execution)
            .await?;
        let replaced = self
            .checkpoint(workspace, CheckpointCause::Restore, None)
            .await?;
        ensure!(
            !replaced.files_omitted,
            "checkpoint {} could not keep the workspace's unsaved files; commit or remove them first",
            replaced.number
        );
        let _git = self.gates.lock_git(workspace.repository()).await;
        git::checkpoint::rewind(
            &workspace.path,
            checkpoint.head.as_str(),
            checkpoint.commit.as_str(),
        )
        .await
        .with_context(|| {
            format!(
                "restore to checkpoint {} failed; checkpoint {} keeps the state before it",
                checkpoint.number, replaced.number
            )
        })?;
        Ok(replaced)
    }

    /// Refuse a rewind that would delete ignored files, disturb another
    /// execution or a Git operation, or switch branches.
    async fn ensure_rewindable(
        &self,
        workspace: &Workspace,
        checkpoint: &Checkpoint,
        own_execution: Option<&ExecutionId>,
    ) -> Result<()> {
        self.ensure_no_other_execution(workspace, own_execution)
            .await?;
        let git_dir = self.verified_git_dir(workspace).await?;
        let path = &workspace.path;
        ensure!(
            !git::status::operation_in_progress_at(path, &git_dir).await?,
            "a Git operation is under way in the workspace; finish or abort it first"
        );
        let branch = git::checked_out_branch(path).await?;
        ensure!(
            checkpoint.branch == branch,
            "checkpoint {} is on {}; the workspace is on {}",
            checkpoint.number,
            branch_label(checkpoint.branch.as_ref()),
            branch_label(branch.as_ref())
        );
        let ignored = git::checkpoint::ignored_collisions(
            path,
            checkpoint.head.as_str(),
            checkpoint.commit.as_str(),
        )
        .await?;
        ensure!(
            ignored.is_empty(),
            "checkpoint {} would overwrite ignored files no checkpoint keeps; move them first: {}",
            checkpoint.number,
            ignored.join(", ")
        );
        Ok(())
    }

    /// Refuse a change to the files of `workspace` while an execution other
    /// than `own` runs in it, naming the command that stops it.
    pub(super) async fn ensure_no_other_execution(
        &self,
        workspace: &Workspace,
        own: Option<&ExecutionId>,
    ) -> Result<()> {
        if let Some(execution) = self.other_execution(&workspace.id, own).await? {
            bail!(
                "execution {execution} runs in the workspace; stop it with shoal stop {} first",
                workspace.number
            );
        }
        Ok(())
    }

    /// A recorded execution in the workspace other than `own`.
    async fn other_execution(
        &self,
        workspace_id: &SpaceId,
        own: Option<&ExecutionId>,
    ) -> Result<Option<ExecutionId>> {
        let (id, own) = (workspace_id.to_owned(), own.cloned());
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT id FROM executions WHERE space_id=?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
                        params![id, own],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Drop the removal record of `removed`, which its restore opened under
    /// its ID, with what keeps its attached files from expiring, and record
    /// the restore, at once; its checkpoints, events and files stay where
    /// they are. The files gate keeps the deletion of the removed space's
    /// files from coming between.
    pub(in crate::daemon) async fn adopt_removal(&self, removed: &RemovedSpace) -> Result<()> {
        let _files = match &removed.repository_id {
            Some(repository) => Some(self.gates.lock_message_files(repository).await),
            None => None,
        };
        let id = removed.space_id.clone();
        self.store
            .write(move |tx| {
                crate::daemon::workspace_files::restore(tx, &id)?;
                crate::daemon::events::record(
                    tx,
                    &id,
                    crate::daemon::events::EventKind::Restored,
                    crate::model::EventCause::Manual,
                    None,
                )?;
                tx.execute("DELETE FROM removed_spaces WHERE space_id=?1", [&id])?;
                Ok(())
            })
            .await
    }

    /// Delete expired checkpoints and removal records, checkpoints and
    /// imported session files no workspace or removal record owns, and refs
    /// without a checkpoint, keep only the file versions code references
    /// still point at, then prune the repository's unreachable loose
    /// objects as its gc would. A failure keeps what it concerns for the
    /// next pass and does not skip the rest.
    pub async fn expire_checkpoints(&self) -> Result<()> {
        let mut failed = Vec::new();
        if let Err(error) = self.store.run(|db| delete_expired_removals(db)).await {
            failed.push(format!("{error:#}"));
        }
        for repo in self.repositories().await? {
            if let Err(error) = self.expire_repository_checkpoints(&repo).await {
                failed.push(format!("{}: {error:#}", repo.path.display()));
            }
        }
        if let Err(error) = self.expire_outside_sessions().await {
            failed.push(format!("root_dir: {error:#}"));
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Delete the session files the configured `root_dir` keeps for chats
    /// without a repository that neither exist nor have a removal record.
    async fn expire_outside_sessions(&self) -> Result<()> {
        let home = HomeKey::Root(self.config().root_dir(&self.paths)?);
        let _refs = self.gates.lock_checkpoints().await;
        self.expire_imported_sessions(&home).await
    }

    /// A busy repository, or one being moved or removed, keeps its refs until
    /// a later pass rather than holding up the others.
    async fn expire_repository_checkpoints(&self, repo: &Repository) -> Result<()> {
        let Some(_git) = self.gates.try_lock_git(&repo.id) else {
            return Ok(());
        };
        if self.ensure_repository_available(&repo.id).await.is_err() {
            return Ok(());
        }
        let repo = &self.repository(&repo.id).await?;
        let retention = self
            .repository_settings(&repo.id)
            .await?
            .cleanup
            .restore
            .retention();
        let _refs = self.gates.lock_checkpoints().await;
        let repository_id = repo.id.clone();
        let kept = self
            .store
            .run(move |db| {
                delete_expired(db, &repository_id)?;
                let before = UnixTime::now() - retention;
                crate::daemon::changes::records::expire(db, &repository_id, before)?;
                kept_commits(db, &repository_id)
            })
            .await?;
        // An unreadable session file keeps no ref.
        let sessions = (self.expire_imported_sessions(&HomeKey::Repository(repo.clone()))).await;
        // A failure leaves the objects to the next pass that deletes a ref.
        let deleted = self.delete_unowned_refs(repo, &kept).await?;
        if self.expire_code_versions(repo).await? || deleted {
            git::checkpoint::prune_unreachable(&repo.path).await?;
        }
        sessions
    }

    /// A ref no checkpoint keeps is left by deleted checkpoints, or by a
    /// write that failed between the record and the ref. `true` when one
    /// was deleted.
    async fn delete_unowned_refs(&self, repo: &Repository, kept: &HashSet<String>) -> Result<bool> {
        let mut deleted = false;
        for (reference, commit) in git::checkpoint::saved_refs(&repo.path).await? {
            if !kept.contains(commit.as_str()) {
                deleted |= git::checkpoint::delete_at(&repo.path, &reference, &commit).await?;
            }
        }
        Ok(deleted)
    }
}

/// Whether checkpoints `kept` and `next` of a workspace of the repository
/// checked out at `repo` keep the same branch, HEAD and files.
async fn keeps_same(repo: &std::path::Path, kept: &Checkpoint, next: &Checkpoint) -> Result<bool> {
    if (&kept.branch, &kept.head, kept.files_omitted)
        != (&next.branch, &next.head, next.files_omitted)
    {
        return Ok(false);
    }
    git::checkpoint::same_files(repo, kept.commit.as_str(), next.commit.as_str()).await
}

fn branch_label(branch: Option<&BranchName>) -> String {
    branch.map_or_else(
        || "a detached HEAD".to_owned(),
        |branch| format!("branch {branch}"),
    )
}

/// Record a checkpoint under the workspace's next number, in the caller's
/// transaction.
pub(in crate::daemon) fn insert_numbered(
    tx: &rusqlite::Transaction,
    checkpoint: &Checkpoint,
) -> Result<CheckpointNumber> {
    // The sequence outlives expired checkpoints, so a number never names a
    // second one.
    let number: CheckpointNumber = tx.query_row(
        "INSERT INTO checkpoint_numbers(workspace_id,last) VALUES (?1,1)
            ON CONFLICT(workspace_id) DO UPDATE SET last=last+1 RETURNING last",
        [&checkpoint.workspace_id],
        |row| row.get(0),
    )?;
    let record = Checkpoint {
        number,
        ..checkpoint.clone()
    };
    tx.execute(
        "INSERT INTO checkpoints(workspace_id,number,repository_id,expires_at,record)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            record.workspace_id,
            number,
            record.repository_id,
            record.expires_at,
            serde_json::to_string(&record)?,
        ],
    )?;
    Ok(record.number)
}

/// Move an unexpired checkpoint's expiry; `false` when it expired meanwhile.
fn extend(db: &Connection, checkpoint: &Checkpoint) -> Result<bool> {
    Ok(db.execute(
        "UPDATE checkpoints SET expires_at=?3, record=?4
            WHERE workspace_id=?1 AND number=?2 AND expires_at > unixepoch()",
        params![
            checkpoint.workspace_id,
            checkpoint.number,
            checkpoint.expires_at,
            serde_json::to_string(checkpoint)?,
        ],
    )? == 1)
}

/// Give `to` the unexpired checkpoints of `from`.
pub(in crate::daemon) fn reassign(db: &Connection, from: &SpaceId, to: &SpaceId) -> Result<()> {
    db.execute(
        "INSERT INTO checkpoint_numbers(workspace_id,last)
            SELECT ?2, last FROM checkpoint_numbers WHERE workspace_id=?1
            ON CONFLICT(workspace_id) DO UPDATE SET last=max(last, excluded.last)",
        params![from, to],
    )?;
    db.execute(
        "DELETE FROM checkpoint_numbers WHERE workspace_id=?1",
        [from],
    )?;
    for checkpoint in workspace_checkpoints(db, from)? {
        let record = Checkpoint {
            workspace_id: to.to_owned(),
            ..checkpoint
        };
        db.execute(
            "UPDATE checkpoints SET workspace_id=?3, record=?4 WHERE workspace_id=?1 AND number=?2",
            params![from, record.number, to, serde_json::to_string(&record)?],
        )?;
    }
    Ok(())
}

/// Delete expired removal records, and the numbering of workspaces that
/// neither exist nor have a removal record or checkpoint left.
fn delete_expired_removals(db: &Connection) -> Result<()> {
    db.execute(
        "DELETE FROM removed_spaces WHERE expires_at <= unixepoch()",
        [],
    )?;
    db.execute(
        "DELETE FROM checkpoint_numbers WHERE workspace_id NOT IN (SELECT id FROM workspaces)
            AND workspace_id NOT IN (SELECT space_id FROM removed_spaces)
            AND workspace_id NOT IN (SELECT workspace_id FROM checkpoints)",
        [],
    )?;
    Ok(())
}

/// Delete the repository's checkpoints that expired, other than a
/// workspace's starting checkpoint, or that neither a workspace nor a
/// removal record owns.
fn delete_expired(db: &Connection, repository_id: &RepositoryId) -> Result<()> {
    db.execute(
        &format!(
            "DELETE FROM checkpoints WHERE repository_id=?1
                AND (NOT {KEPT} OR (workspace_id NOT IN (SELECT id FROM workspaces)
                    AND workspace_id NOT IN (SELECT space_id FROM removed_spaces)))"
        ),
        [repository_id],
    )?;
    Ok(())
}

/// The commits the repository's checkpoints keep, read once so a pass over
/// thousands of refs does not query the store for each.
fn kept_commits(db: &Connection, repository_id: &RepositoryId) -> Result<HashSet<String>> {
    db.prepare("SELECT json_extract(record, '$.commit') FROM checkpoints WHERE repository_id=?1")?
        .query_map([repository_id], |row| row.get(0))?
        .map(|commit| Ok(commit?))
        .collect()
}

fn latest(db: &Connection, workspace_id: &SpaceId) -> Result<Option<Checkpoint>> {
    let record: Option<String> = db
        .query_row(
            &format!(
                "SELECT record FROM checkpoints WHERE workspace_id=?1 AND {KEPT}
                    ORDER BY number DESC LIMIT 1"
            ),
            [workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// An unexpired checkpoint of the workspace.
pub(super) fn find(
    db: &Connection,
    workspace_id: &SpaceId,
    number: CheckpointNumber,
) -> Result<Option<Checkpoint>> {
    let record: Option<String> = db
        .query_row(
            &format!(
                "SELECT record FROM checkpoints WHERE workspace_id=?1 AND number=?2 AND {KEPT}"
            ),
            params![workspace_id, number],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// Whether a checkpoint is kept: it has not expired, or it is its
/// workspace's starting checkpoint, which lasts as long as the workspace,
/// or the removal checkpoint its restore still creates the worktree from.
const KEPT: &str = "(expires_at > unixepoch() OR EXISTS (SELECT 1 FROM workspaces
    WHERE id=checkpoints.workspace_id AND starting_checkpoint=checkpoints.number)
    OR EXISTS (SELECT 1 FROM workspaces WHERE id=checkpoints.workspace_id
        AND json_extract(pending_plan,'$.source.kind')='restore'
        AND json_extract(pending_plan,'$.source.checkpoint')=checkpoints.number))";

/// The record bytes of one listing page, leaving room in a protocol frame
/// for the envelope; [`NAME_LIMIT`] keeps one record well below it.
const PAGE_BYTES: usize = crate::protocol::MAX_FRAME / 2;
/// The longest checkpoint name, in characters.
const NAME_LIMIT: usize = 200;

/// The checkpoints numbered after `after`, oldest first, as many as fit in
/// [`PAGE_BYTES`] and at least one.
fn checkpoint_page(
    db: &Connection,
    workspace_id: &SpaceId,
    after: CheckpointNumber,
) -> Result<Vec<Checkpoint>> {
    let mut statement = db.prepare(&format!(
        "SELECT record FROM checkpoints WHERE workspace_id=?1 AND number > ?2 AND {KEPT}
            ORDER BY number"
    ))?;
    let mut rows = statement.query(params![workspace_id, after])?;
    let (mut page, mut bytes) = (Vec::new(), 0);
    while let Some(row) = rows.next()? {
        let record: String = row.get(0)?;
        bytes += record.len() + 1;
        if bytes > PAGE_BYTES && !page.is_empty() {
            break;
        }
        page.push(serde_json::from_str(&record)?);
    }
    Ok(page)
}

/// The workspace's unexpired checkpoints, oldest first.
fn workspace_checkpoints(db: &Connection, workspace_id: &SpaceId) -> Result<Vec<Checkpoint>> {
    db.prepare(&format!(
        "SELECT record FROM checkpoints WHERE workspace_id=?1 AND {KEPT} ORDER BY number"
    ))?
    .query_map([workspace_id], |row| row.get::<_, String>(0))?
    .map(|record| Ok(serde_json::from_str(&record?)?))
    .collect()
}

#[cfg(test)]
mod tests;
