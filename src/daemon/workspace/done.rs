//! Assignment completion is a signal; the cleanup sweep owns removal.
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

use super::{GuardMode, Manager, ResourceGuard};
use crate::{
    config::Effective,
    daemon::{events::EventCause, notifications::NotificationKind, questions, store},
    hooks::{self, Hook, HookKind},
    model::{Completion, HandedBack, Workspace},
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

    /// Withdraw a recorded completion; only holds and open questions keep the
    /// workspace.
    pub async fn undo_done(&self, selector: &str) -> Result<Completion> {
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        let completion = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &workspace.id)?;
                let record: String = tx
                    .query_row(
                        "DELETE FROM workspace_completion WHERE workspace_id=?1 RETURNING record",
                        [&workspace.id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .context("assignment is not marked done")?;
                // An unreadable record stays in place for diagnosis.
                let completion = serde_json::from_str(&record)?;
                tx.commit()?;
                Ok(completion)
            })
            .await?;
        self.signals.wake_cleanup();
        Ok(completion)
    }

    /// `cleanup` requests cleanup even when `[cleanup.done] enabled` is false;
    /// `integrated` records that another branch has HEAD's work under other
    /// commits; copies of the files `results` are handed back, which the
    /// workspace's attached files keep after its removal.
    pub async fn mark_done(
        &self,
        selector: &str,
        cleanup: bool,
        integrated: bool,
        results: Vec<PathBuf>,
    ) -> Result<Completion> {
        crate::daemon::workspace_files::check_results(&results)?;
        // Serialize cleanup choices with the PR and completion sweeps.
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        let head = crate::daemon::links::pr::current_head(&workspace).await?;
        let results = self.keep_results(&workspace, results).await?;
        self.record_done(&workspace, head, cleanup, integrated, results)
            .await
    }

    /// Caller holds the workspace's PR gate and has verified ownership and
    /// this HEAD.
    async fn record_done(
        &self,
        workspace: &Workspace,
        head: String,
        cleanup: bool,
        integrated: bool,
        results: Vec<HandedBack>,
    ) -> Result<Completion> {
        let settings = self.workspace_settings(workspace).await?;
        let hook = self.done_hook(workspace, &settings).await?;
        self.verify_worktree(workspace).await?;
        let completion = Completion {
            head,
            cleanup: cleanup || settings.cleanup.done.enabled,
            integrated,
            results,
            error: None,
        };
        let persisted = self.persist_completion(&workspace.id, &completion).await?;
        // Rerunning the hook is the one effect a repeated done may have left.
        ensure!(
            persisted.changed || hook.is_some(),
            "assignment is already marked done at this HEAD"
        );
        let closed = persisted.closed;
        self.forget_last_sessions(&workspace.id);
        // A done assignment needs no agent; only naming the workspace in
        // `shoal resume` restores it now.
        if let Err(error) = self.cancel_stopped_restores(&workspace.id).await {
            crate::daemon::log!("agent queue: {error:#}");
        }
        self.notify(
            Some(&workspace.name),
            NotificationKind::WorkspaceDone,
            done_notice(&workspace.name, &completion, &closed),
        )
        .await;
        if let Some((command, _resources)) = hook {
            self.run_done_hook(workspace, &command, &completion).await;
        }
        self.signals.wake_cleanup();
        Ok(completion)
    }

    /// Record `completion` and close the workspace's open questions, which
    /// the finished assignment waits on no longer, waking waits on them.
    async fn persist_completion(&self, id: &str, completion: &Completion) -> Result<Persisted> {
        let id = id.to_owned();
        let completion = completion.clone();
        let persisted = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                store::require_ready(&tx, &id)?;
                let replaced = record_completion(&tx, &id, &completion)?;
                // Like the continuation it replaced, a hold made from one by
                // migration 31 ends at the next explicit done.
                let released = tx.execute(
                    "DELETE FROM workspace_holds WHERE workspace_id=?1 AND from_continuation",
                    [&id],
                )?;
                let closed = questions::close_open(&tx, &id)?;
                // A finished assignment leaves its failed turn alone.
                super::turn_retry::cancel(&tx, &id)?;
                tx.commit()?;
                Ok(Persisted {
                    changed: replaced || released > 0 || !closed.is_empty(),
                    closed,
                })
            })
            .await?;
        if !persisted.closed.is_empty() {
            self.signals.question_changed();
        }
        Ok(persisted)
    }

    /// The configured `hooks.post_done`, holding off permit and lifecycle changes
    /// until it has run.
    async fn done_hook(
        &self,
        workspace: &Workspace,
        settings: &Effective,
    ) -> Result<Option<(PathBuf, ResourceGuard)>> {
        let Some(path) = HookKind::PostDone.command(settings) else {
            return Ok(None);
        };
        let guard = self
            .gates
            .resource_guard(&workspace.id, GuardMode::Exclusive)?;
        Ok(Some((workspace.path.join(path), guard)))
    }

    /// Failure only notifies; it never undoes completion.
    async fn run_done_hook(&self, workspace: &Workspace, command: &Path, completion: &Completion) {
        let result = async {
            let configured_env = self.configured_workspace_environment(workspace).await?;
            hooks::run_detached(
                Hook::PostDone(completion),
                workspace,
                command,
                &self.paths,
                &configured_env,
            )
            .await
        }
        .await;
        if let Err(error) = result {
            self.notify(
                Some(&workspace.name),
                NotificationKind::HookFailed,
                format!("assignment finished; {error:#}"),
            )
            .await;
        }
    }

    /// A completion without cleanup also blocks older PR watches. A completion
    /// must never authorize removal of a later revision of the assignment.
    pub(crate) async fn completion_allows_cleanup(&self, workspace: &Workspace) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(true);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        ensure!(
            crate::daemon::links::pr::current_head(workspace).await? == completion.head,
            "HEAD changed after the assignment was marked done; retaining workspace"
        );
        Ok(true)
    }

    /// Each workspace's PR gate is held only while that workspace is handled.
    /// A workspace below a completed one goes by that completion.
    pub async fn sweep_completed(&self) -> Result<()> {
        // A failure in one workspace, such as an unreadable completion of
        // its ancestor, does not skip the others.
        let mut failed = Vec::new();
        for listed in self.list_workspaces().await? {
            if let Err(error) = self.sweep_completed_workspace(&listed).await {
                failed.push(format!("{}: {error:#}", listed.name));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Remove `listed` for its own completion or a completed ancestor's.
    async fn sweep_completed_workspace(&self, listed: &Workspace) -> Result<()> {
        // An ancestor's completion that takes the workspace changes under
        // the ancestor's gate, so that one is held too.
        let gated = self.completed_ancestor(listed).await?;
        let gates = std::iter::once(&listed.id).chain(gated.as_ref().map(|a| &a.id));
        let _guard = self.gates.lock_prs(gates).await;
        let Some(workspace) = self.find_workspace(&listed.id).await? else {
            return Ok(());
        };
        if workspace.state != WorkspaceState::Ready {
            return Ok(());
        }
        if let Some(ancestor) = self.completed_ancestor(&workspace).await? {
            // Another ancestor's completion waits for the next sweep, which
            // holds its gate.
            if gated.is_some_and(|gated| gated.id == ancestor.id) {
                self.sweep_under_completed(&workspace, &ancestor).await?;
            }
            return Ok(());
        }
        match self.cleanup_completed(&workspace).await {
            Ok(true) => {
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::WorkspaceRemoved,
                    super::removal_notice(
                        &workspace.name,
                        "removed after the assignment was marked done",
                    ),
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
        Ok(())
    }

    async fn cleanup_completed(&self, workspace: &Workspace) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(false);
        };
        if !completion.cleanup || self.keep_reason(&workspace.id).await?.is_some() {
            return Ok(false);
        }
        // A pre-existing PR registration keeps its own merge requirements.
        if self.pr_registration(&workspace.id).await?.is_some() {
            return Ok(false);
        }
        let cause = self.completion_cause(&workspace.id).await?;
        self.remove_completed(&workspace.id, &completion, cause)
            .await
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

/// What recording a completion changed.
struct Persisted {
    /// Whether it replaced another completion, released a hold or closed a
    /// question.
    changed: bool,
    /// The numbers of the questions it closed.
    closed: Vec<i64>,
}

/// Record `completion`, returning whether it differs from the recorded one:
/// another HEAD, cleanup choice, integration or cleanup error, or an
/// unreadable record.
fn record_completion(
    tx: &rusqlite::Transaction,
    id: &str,
    completion: &Completion,
) -> Result<bool> {
    let recorded: Option<String> = tx
        .query_row(
            "SELECT record FROM workspace_completion WHERE workspace_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let recorded = recorded.and_then(|record| serde_json::from_str::<Completion>(&record).ok());
    tx.execute(
        "INSERT INTO workspace_completion(workspace_id,record,cause) VALUES (?1,?2,?3)
         ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record,cause=excluded.cause",
        rusqlite::params![id, serde_json::to_string(completion)?, EventCause::Manual],
    )?;
    Ok(recorded.as_ref() != Some(completion))
}

/// What the user is told about a completion, the questions it closed and
/// the files it handed back.
fn done_notice(workspace: &str, completion: &Completion, closed: &[i64]) -> String {
    let mut notice = String::from("assignment finished");
    if completion.integrated {
        notice.push_str("; integrated elsewhere");
    }
    notice.push_str(if completion.cleanup {
        "; cleanup requested"
    } else {
        "; no cleanup requested"
    });
    if let [number] = closed {
        notice.push_str(&format!("; closed question {number}"));
    } else if !closed.is_empty() {
        let numbers: Vec<_> = closed.iter().map(ToString::to_string).collect();
        notice.push_str(&format!("; closed questions {}", numbers.join(", ")));
    }
    if !completion.results.is_empty() {
        notice.push_str(&format!(
            "; results in shoal files {workspace}: {}",
            results_preview(&completion.results)
        ));
    }
    notice
}

/// The results a notice names: the first few, then how many more, since
/// `shoal files` lists them all.
fn results_preview(results: &[HandedBack]) -> String {
    const NAMED: usize = 3;
    let names: Vec<_> = results
        .iter()
        .take(NAMED)
        .map(|result| result.name.as_str())
        .collect();
    match results.len().saturating_sub(NAMED) {
        0 => names.join(", "),
        more => format!("{} and {more} more", names.join(", ")),
    }
}

mod children;

#[cfg(test)]
mod tests;
