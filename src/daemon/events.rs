//! Durable lifecycle records, independent of notification delivery and read state.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::workspace::Manager;
use crate::state::states;

states!(EventKind {
    Created => "created",
    Ready => "ready",
    SetupFailed => "setup_failed",
    Completed => "completed",
    Continued => "continued",
    Removed => "removed",
    Retained => "retained",
    BranchChanged => "branch_changed",
});

states!(EventCause {
    Manual => "manual",
    Idle => "idle",
    Issue => "issue",
    Pr => "pr",
    Completion => "completion",
    MissingDirectory => "missing_directory",
});

/// Append in the transaction that owns the transition, before deleting ownership.
pub(crate) fn record(
    db: &Connection,
    id: &str,
    kind: EventKind,
    cause: EventCause,
    error: Option<&str>,
) -> Result<()> {
    // Repeated cleanup failures are unchanged state, even when notifications are read.
    db.execute(
        "INSERT INTO workspace_events(record)
         SELECT json_object('kind',?2,'workspace_id',id,'repository_id',repository_id,
             'name',name,'path',path,'branch',branch,'cause',?3,'error',?4)
         FROM workspaces WHERE id=?1 AND NOT EXISTS (
             SELECT 1 FROM workspace_events WHERE id=(
                 SELECT MAX(id) FROM workspace_events WHERE json_extract(record,'$.workspace_id')=?1
             ) AND json_extract(record,'$.kind')=?2 AND json_extract(record,'$.cause')=?3
                 AND json_extract(record,'$.error') IS ?4
         )",
        params![id, kind, cause, error],
    )?;
    Ok(())
}

impl Manager {
    /// Observe external checkouts without changing the branch Shoal owns.
    pub(crate) async fn observe_workspace_branch(
        &self,
        workspace: &crate::model::Workspace,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        let head = crate::git::run(
            &workspace.path,
            &["rev-parse", "--symbolic-full-name", "HEAD"],
        )
        .await?;
        let branch = crate::git::strip_local(head.trim_end()).map(str::to_owned);
        let id = workspace.id.clone();
        self.store.run(move |db| {
            db.execute("UPDATE workspaces SET observed_branch=?2 WHERE id=?1 AND observed_branch IS NOT ?2", params![id, branch])?;
            Ok(())
        }).await
    }

    pub(crate) async fn retain_workspace(
        &self,
        id: &str,
        state: crate::state::WorkspaceState,
        cause: EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
                    params![id, state, error],
                )?;
                record(&tx, &id, EventKind::Retained, cause, Some(&error))?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn record_retained(
        &self,
        id: &str,
        cause: EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| record(db, &id, EventKind::Retained, cause, Some(&error)))
            .await
    }
}
