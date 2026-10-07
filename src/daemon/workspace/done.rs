//! Assignment completion is a signal; the cleanup sweep owns removal.
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;

use super::{GuardMode, Manager};
use crate::{
    daemon::{events::EventCause, notifications::NotificationKind, store},
    hooks::{self, Hook, HookKind},
    model::{Completion, Workspace},
    state::WorkspaceState,
};

impl Manager {
    pub async fn completion(&self, id: &str) -> Result<Option<Completion>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM workspace_completion WHERE workspace_id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?;
                record
                    .map(|record| serde_json::from_str(&record).map_err(Into::into))
                    .transpose()
            })
            .await
    }

    pub async fn manual_completion(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM workspace_continuation WHERE workspace_id=?1",
                    [id],
                )
            })
            .await
    }

    /// Keep an unfinished assignment alive until an explicit done signal.
    pub async fn continue_workspace(&self, selector: &str) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace.id)?;
                tx.execute(
                    "INSERT INTO workspace_continuation(workspace_id) VALUES (?1)
                     ON CONFLICT(workspace_id) DO NOTHING",
                    [&workspace.id],
                )?;
                tx.execute(
                    "DELETE FROM workspace_completion WHERE workspace_id=?1",
                    [&workspace.id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn mark_done(&self, selector: &str, cleanup: Option<bool>) -> Result<Completion> {
        // Serialize keep/cleanup choices with automatic completion sweeps.
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let head = crate::forge::pr::current_head(&workspace).await?;
        self.record_done(&workspace, head, cleanup, EventCause::Manual)
            .await
    }

    /// Caller holds the PR gate and has verified ownership and this HEAD.
    pub(crate) async fn record_done(
        &self,
        workspace: &Workspace,
        head: String,
        cleanup: Option<bool>,
        cause: EventCause,
    ) -> Result<Completion> {
        let settings = self.workspace_settings(workspace).await?;
        let command = HookKind::PostDone
            .command(&settings)
            .map(|path| workspace.path.join(path));
        let _resources = if command.is_some() {
            Some(
                self.resource_guard(&workspace.id, GuardMode::Exclusive)
                    .await?,
            )
        } else {
            None
        };
        self.verify_worktree(workspace).await?;
        let cleanup = cleanup.unwrap_or(settings.done.cleanup);
        let completion = Completion {
            head,
            cleanup,
            error: None,
        };
        let id = workspace.id.clone();
        let record = serde_json::to_string(&completion)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                tx.execute(
                    "INSERT INTO workspace_completion(workspace_id,record,cause) VALUES (?1,?2,?3)
                 ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record,cause=excluded.cause",
                    rusqlite::params![id, record, cause],
                )?;
                tx.execute(
                    "DELETE FROM workspace_continuation WHERE workspace_id=?1",
                    [&id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.notify(
            Some(&workspace.name),
            NotificationKind::WorkspaceDone,
            if cleanup {
                "assignment finished; cleanup requested"
            } else {
                "assignment finished; workspace kept for review"
            },
        )
        .await;
        if let Some(command) = command
            && let Err(error) = hooks::run_detached(
                Hook::PostDone(&completion),
                workspace,
                &command,
                &self.paths,
            )
            .await
        {
            self.notify(
                Some(&workspace.name),
                NotificationKind::HookFailed,
                format!("assignment finished; {error:#}"),
            )
            .await;
        }
        self.cleanup_notify.notify_one();
        Ok(completion)
    }

    /// A keep choice also blocks older PR watches. A completion must never
    /// authorize removal of a later revision of the assignment.
    pub(crate) async fn completion_allows_cleanup(&self, workspace: &Workspace) -> Result<bool> {
        if self.manual_completion(&workspace.id).await? {
            return Ok(false);
        }
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(true);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        ensure!(
            crate::forge::pr::current_head(workspace).await? == completion.head,
            "HEAD changed after the assignment was marked done; retaining workspace"
        );
        Ok(true)
    }

    pub async fn sweep_completed(&self) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        for workspace in self.list_workspaces().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            let result = self.cleanup_completed(&workspace).await;
            match result {
                Ok(true) => {
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::WorkspaceRemoved,
                        "removed after the assignment was marked done",
                    )
                    .await
                }
                Ok(false) => {}
                Err(error) => {
                    let cause = self.completion_cause(&workspace.id).await?;
                    self.record_retained(&workspace.id, cause, &error).await?;
                    self.record_completion_error(&workspace.id, &error).await?;
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::CleanupFailed,
                        format!("completion cleanup retained the workspace: {error:#}"),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn cleanup_completed(&self, workspace: &Workspace) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(false);
        };
        if !completion.cleanup || self.has_holds(&workspace.id).await? {
            return Ok(false);
        }
        // A pre-existing PR registration keeps its own merge requirements.
        if self.pr_registration(&workspace.id).await?.is_some() {
            return Ok(false);
        }
        let cause = self.completion_cause(&workspace.id).await?;
        self.remove_completed(&workspace.id, &completion.head, cause)
            .await?;
        Ok(true)
    }

    pub(super) async fn completion_cause(&self, id: &str) -> Result<EventCause> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT cause FROM workspace_completion WHERE workspace_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(EventCause::Completion))
            })
            .await
    }

    async fn record_completion_error(&self, id: &str, error: &anyhow::Error) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| {
                // Preserve malformed records for diagnosis instead of overwriting them.
                db.execute(
                    "UPDATE workspace_completion SET record=json_set(record, '$.error', ?2)
                WHERE workspace_id=?1 AND json_valid(record)",
                    rusqlite::params![id, error],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
