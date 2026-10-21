//! Checkpoints: a workspace's HEAD and unsaved files, kept under private refs
//! until they expire, which `shoal restore` returns the workspace to.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{
    daemon::{log, scope::Caller},
    git,
    model::{
        Checkpoint, CheckpointCause, RemovedWorkspace, Repository, RestoredCheckpoint, Workspace,
    },
    state::WorkspaceState,
    time::unix_seconds,
};

impl Manager {
    /// `shoal checkpoint`: keep the workspace's current state.
    pub async fn take_checkpoint(
        &self,
        selector: &str,
        name: Option<String>,
    ) -> Result<Checkpoint> {
        let workspace = self.workspace(selector).await?;
        ensure!(
            matches!(
                workspace.state,
                WorkspaceState::Ready | WorkspaceState::Stopping | WorkspaceState::Failed
            ),
            "workspace is {}; take a checkpoint once that operation finishes",
            workspace.state
        );
        self.verify_worktree(&workspace).await?;
        self.checkpoint(&workspace, CheckpointCause::Manual, name)
            .await
    }

    /// Keep the state an agent left when it exits or is stopped. A workspace
    /// in another lifecycle operation or removed meanwhile takes none, and a
    /// failure is only logged.
    pub(crate) async fn checkpoint_after_agent(&self, workspace_id: &str) {
        let Ok(workspace) = self.workspace(workspace_id).await else {
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

    /// Keep HEAD and, within the archive limit, the unsaved files reachable
    /// under a new checkpoint. Other than a manual checkpoint, one that would
    /// keep what the latest already keeps extends that one instead.
    pub(super) async fn checkpoint(
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
        let path = &workspace.path;
        let head = git::revision(path, "HEAD", git::run)
            .await?
            .trim()
            .to_owned();
        let files_omitted = !self.archives_unsaved_files(workspace).await?;
        let message = format!("Shoal checkpoint of {}", workspace.name);
        let commit = git::checkpoint::snapshot(path, &message, !files_omitted).await?;
        let created_at = unix_seconds();
        Ok(Checkpoint {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            number: 0,
            name,
            cause,
            branch: current_branch(path).await?,
            head,
            commit,
            files_omitted,
            created_at,
            expires_at: created_at + retention.as_secs(),
        })
    }

    /// Record a snapshot under the workspace's next number, then point its
    /// ref at the commit; a ref that cannot be written takes the record back.
    async fn save_checkpoint(&self, mut checkpoint: Checkpoint) -> Result<Checkpoint> {
        let repo = self.repository(&checkpoint.repository_id).await?;
        let _refs = self.checkpoint_gate.lock().await;
        let record = checkpoint.clone();
        checkpoint.number = self.store.run(move |db| insert(db, &record)).await?;
        let reference = git::checkpoint::reference(&checkpoint.commit);
        if let Err(error) = git::checkpoint::save(&repo.path, &reference, &checkpoint.commit).await
        {
            let (id, number) = (checkpoint.workspace_id.clone(), checkpoint.number);
            self.store.run(move |db| delete(db, &id, number)).await?;
            return Err(error);
        }
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
        if (&latest.branch, &latest.head, latest.files_omitted)
            != (&next.branch, &next.head, next.files_omitted)
            || !git::checkpoint::same_files(&workspace.path, &latest.commit, &next.commit).await?
        {
            return Ok(None);
        }
        latest.expires_at = latest.expires_at.max(next.expires_at);
        let record = latest.clone();
        let extended = self.store.run(move |db| extend(db, &record)).await?;
        Ok(extended.then_some(latest))
    }

    /// The unexpired checkpoints of an existing workspace, oldest first;
    /// `None` when no workspace has that name or ID.
    pub async fn checkpoints(&self, selector: &str) -> Result<Option<Vec<Checkpoint>>> {
        let Ok(workspace) = self.workspace(selector).await else {
            return Ok(None);
        };
        let id = workspace.id;
        self.store
            .run(move |db| workspace_checkpoints(db, &id))
            .await
            .map(Some)
    }

    /// Return the workspace to a checkpoint after keeping its current state
    /// in another. Nothing else may run in it meanwhile, except the caller's
    /// own execution.
    pub async fn restore_checkpoint(
        &self,
        selector: &str,
        number: u64,
        caller: Option<&Caller>,
    ) -> Result<RestoredCheckpoint> {
        let workspace = self.workspace(selector).await?;
        let (id, name) = (workspace.id.clone(), workspace.name.clone());
        let checkpoint = self
            .store
            .run(move |db| find(db, &id, number))
            .await?
            .with_context(|| {
                format!(
                    "workspace {name} has no checkpoint {number}; shoal restore {name} lists them"
                )
            })?;
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
            workspace: self.workspace(&workspace.id).await?,
            checkpoint,
            replaced,
        })
    }

    /// Keep the current state and move the worktree to `checkpoint`.
    async fn rewind(
        &self,
        workspace: &Workspace,
        checkpoint: &Checkpoint,
        own_execution: Option<&str>,
    ) -> Result<Checkpoint> {
        self.ensure_rewindable(workspace, checkpoint, own_execution)
            .await?;
        let replaced = self
            .checkpoint(workspace, CheckpointCause::Restore, None)
            .await?;
        let _git = self.lock_repository_git(&workspace.repository_id).await;
        git::checkpoint::rewind(&workspace.path, &checkpoint.head, &checkpoint.commit)
            .await
            .with_context(|| {
                format!(
                    "restore to checkpoint {} failed; checkpoint {} keeps the state before it",
                    checkpoint.number, replaced.number
                )
            })?;
        Ok(replaced)
    }

    /// Refuse a rewind that would lose unsaved files, disturb another
    /// execution or a Git operation, or switch branches.
    async fn ensure_rewindable(
        &self,
        workspace: &Workspace,
        checkpoint: &Checkpoint,
        own_execution: Option<&str>,
    ) -> Result<()> {
        if let Some(execution) = self.other_execution(&workspace.id, own_execution).await? {
            bail!(
                "execution {execution} runs in the workspace; stop it with shoal stop {} first",
                workspace.name
            );
        }
        self.verify_worktree(workspace).await?;
        let path = &workspace.path;
        ensure!(
            !git::status::operation_in_progress(path).await?,
            "a Git operation is under way in the workspace; finish or abort it first"
        );
        let branch = current_branch(path).await?;
        ensure!(
            checkpoint.branch == branch,
            "checkpoint {} is on {}; the workspace is on {}",
            checkpoint.number,
            branch_label(checkpoint.branch.as_deref()),
            branch_label(branch.as_deref())
        );
        ensure!(
            self.archives_unsaved_files(workspace).await?,
            "a checkpoint cannot keep the workspace's unsaved files; commit or remove them first"
        );
        let ignored =
            git::checkpoint::ignored_collisions(path, &checkpoint.head, &checkpoint.commit).await?;
        ensure!(
            ignored.is_empty(),
            "checkpoint {} would overwrite ignored files no checkpoint keeps; move them first: {}",
            checkpoint.number,
            ignored.join(", ")
        );
        Ok(())
    }

    /// A recorded execution in the workspace other than `own`.
    async fn other_execution(
        &self,
        workspace_id: &str,
        own: Option<&str>,
    ) -> Result<Option<String>> {
        let (id, own) = (workspace_id.to_owned(), own.map(str::to_owned));
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT id FROM executions WHERE workspace_id=?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
                        params![id, own],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Give a reopened workspace the checkpoints of the removed one and drop
    /// its removal record, at once, so the checkpoints always have an owner.
    pub(super) async fn adopt_removal(
        &self,
        removed: &RemovedWorkspace,
        workspace_id: &str,
    ) -> Result<()> {
        let (from, to) = (removed.workspace_id.clone(), workspace_id.to_owned());
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                reassign(&tx, &from, &to)?;
                tx.execute(
                    "DELETE FROM removed_workspaces WHERE workspace_id=?1",
                    [&from],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Delete expired checkpoints and removal records, checkpoints no
    /// workspace or removal record owns, and refs without a checkpoint. A
    /// failure keeps what it concerns for the next pass and does not skip
    /// the rest.
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
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_repository_checkpoints(&self, repo: &Repository) -> Result<()> {
        let _refs = self.checkpoint_gate.lock().await;
        let repository_id = repo.id.clone();
        self.store
            .run(move |db| delete_expired(db, &repository_id))
            .await?;
        self.adopt_legacy_refs(repo).await?;
        self.delete_unowned_refs(repo).await
    }

    /// A ref no checkpoint keeps is left by deleted checkpoints, or by a
    /// write that failed between the record and the ref.
    async fn delete_unowned_refs(&self, repo: &Repository) -> Result<()> {
        for (reference, commit) in git::checkpoint::saved_refs(&repo.path).await? {
            if !self.keeps_commit(repo, &commit).await? {
                git::checkpoint::delete_at(&repo.path, &reference, &commit).await?;
            }
        }
        Ok(())
    }

    /// Move the refs removals kept before checkpoints to the checkpoint ref
    /// of the commit when the store migration recorded a checkpoint for it,
    /// deleting the rest.
    async fn adopt_legacy_refs(&self, repo: &Repository) -> Result<()> {
        for (legacy, commit) in git::checkpoint::legacy_refs(&repo.path).await? {
            if self.keeps_commit(repo, &commit).await? {
                let reference = git::checkpoint::reference(&commit);
                git::checkpoint::save(&repo.path, &reference, &commit).await?;
            }
            git::checkpoint::delete_at(&repo.path, &legacy, &commit).await?;
        }
        Ok(())
    }

    /// Whether a checkpoint of the repository keeps `commit`.
    async fn keeps_commit(&self, repo: &Repository, commit: &str) -> Result<bool> {
        let (id, commit) = (repo.id.clone(), commit.to_owned());
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM checkpoints
                     WHERE repository_id=?1 AND json_extract(record, '$.commit')=?2",
                    [id, commit],
                )
            })
            .await
    }
}

/// The branch checked out; `None` for a detached HEAD.
async fn current_branch(path: &std::path::Path) -> Result<Option<String>> {
    let branch = git::current_branch(path).await?.trim().to_owned();
    Ok((!branch.is_empty()).then_some(branch))
}

fn branch_label(branch: Option<&str>) -> String {
    branch.map_or_else(
        || "a detached HEAD".to_owned(),
        |branch| format!("branch {branch}"),
    )
}

/// Record a checkpoint under the workspace's next number and return it.
fn insert(db: &mut Connection, checkpoint: &Checkpoint) -> Result<u64> {
    let tx = db.transaction()?;
    let number: i64 = tx.query_row(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM checkpoints WHERE workspace_id=?1",
        [&checkpoint.workspace_id],
        |row| row.get(0),
    )?;
    let record = Checkpoint {
        number: number as u64,
        ..checkpoint.clone()
    };
    tx.execute(
        "INSERT INTO checkpoints(workspace_id,number,repository_id,expires_at,record)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            record.workspace_id,
            number,
            record.repository_id,
            record.expires_at as i64,
            serde_json::to_string(&record)?,
        ],
    )?;
    tx.commit()?;
    Ok(record.number)
}

/// Move an unexpired checkpoint's expiry; `false` when it expired meanwhile.
fn extend(db: &Connection, checkpoint: &Checkpoint) -> Result<bool> {
    Ok(db.execute(
        "UPDATE checkpoints SET expires_at=?3, record=?4
            WHERE workspace_id=?1 AND number=?2 AND expires_at > unixepoch()",
        params![
            checkpoint.workspace_id,
            checkpoint.number as i64,
            checkpoint.expires_at as i64,
            serde_json::to_string(checkpoint)?,
        ],
    )? == 1)
}

/// Give `to` the unexpired checkpoints of `from`.
fn reassign(db: &Connection, from: &str, to: &str) -> Result<()> {
    for checkpoint in workspace_checkpoints(db, from)? {
        let record = Checkpoint {
            workspace_id: to.to_owned(),
            ..checkpoint
        };
        db.execute(
            "UPDATE checkpoints SET workspace_id=?3, record=?4 WHERE workspace_id=?1 AND number=?2",
            params![
                from,
                record.number as i64,
                to,
                serde_json::to_string(&record)?
            ],
        )?;
    }
    Ok(())
}

fn delete_expired_removals(db: &Connection) -> Result<()> {
    db.execute(
        "DELETE FROM removed_workspaces WHERE expires_at <= unixepoch()",
        [],
    )?;
    Ok(())
}

fn delete(db: &Connection, workspace_id: &str, number: u64) -> Result<()> {
    db.execute(
        "DELETE FROM checkpoints WHERE workspace_id=?1 AND number=?2",
        params![workspace_id, number as i64],
    )?;
    Ok(())
}

/// Delete the repository's checkpoints that expired or that neither a
/// workspace nor a removal record owns.
fn delete_expired(db: &Connection, repository_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM checkpoints WHERE repository_id=?1
            AND (expires_at <= unixepoch() OR (workspace_id NOT IN (SELECT id FROM workspaces)
                AND workspace_id NOT IN (SELECT workspace_id FROM removed_workspaces)))",
        [repository_id],
    )?;
    Ok(())
}

fn latest(db: &Connection, workspace_id: &str) -> Result<Option<Checkpoint>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM checkpoints WHERE workspace_id=?1 AND expires_at > unixepoch()
                ORDER BY number DESC LIMIT 1",
            [workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// An unexpired checkpoint of the workspace.
fn find(db: &Connection, workspace_id: &str, number: u64) -> Result<Option<Checkpoint>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM checkpoints WHERE workspace_id=?1 AND number=?2
                AND expires_at > unixepoch()",
            params![workspace_id, number as i64],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// The workspace's unexpired checkpoints, oldest first.
fn workspace_checkpoints(db: &Connection, workspace_id: &str) -> Result<Vec<Checkpoint>> {
    db.prepare(
        "SELECT record FROM checkpoints WHERE workspace_id=?1 AND expires_at > unixepoch()
            ORDER BY number",
    )?
    .query_map([workspace_id], |row| row.get::<_, String>(0))?
    .map(|record| Ok(serde_json::from_str(&record?)?))
    .collect()
}

#[cfg(test)]
mod tests;
