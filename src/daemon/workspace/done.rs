//! Assignment completion is a signal; the cleanup sweep owns removal.
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;

use super::{GuardMode, Manager};
use crate::{
    daemon::{notifications::NotificationKind, store},
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

    pub async fn mark_done(&self, selector: &str, cleanup: Option<bool>) -> Result<Completion> {
        // Serialize keep/cleanup choices with automatic completion sweeps.
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let head = crate::forge::pr::current_head(&workspace).await?;
        self.record_done(&workspace, head, cleanup).await
    }

    /// Caller holds the PR gate and has verified ownership and this HEAD.
    pub(crate) async fn record_done(
        &self,
        workspace: &Workspace,
        head: String,
        cleanup: Option<bool>,
    ) -> Result<Completion> {
        let settings = self.workspace_settings(workspace).await?;
        let command = HookKind::PostDone
            .command(&settings)
            .map(|path| workspace.path.join(path));
        let mode = if command.is_some() {
            GuardMode::Exclusive
        } else {
            GuardMode::Shared
        };
        let _resources = self.resource_guard(&workspace.id, mode).await?;
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
                    "INSERT INTO workspace_completion(workspace_id,record) VALUES (?1,?2)
                 ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record",
                    rusqlite::params![id, record],
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
        if !completion.cleanup {
            return Ok(false);
        }
        // A pre-existing PR registration keeps its own merge requirements.
        if self.pr_registration(&workspace.id).await?.is_some() {
            return Ok(false);
        }
        self.remove_completed(&workspace.id, &completion.head)
            .await?;
        Ok(true)
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
