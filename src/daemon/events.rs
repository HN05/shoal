//! Durable lifecycle records, independent of notification delivery and read state.
use std::path::PathBuf;

use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

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
    /// A workspace process marked linked work ready for review.
    ReviewReady => "review_ready",
    /// A ready mark was withdrawn or its item unlinked.
    ReviewCleared => "review_cleared",
});

states!(EventCause {
    Manual => "manual",
    Idle => "idle",
    Issue => "issue",
    Pr => "pr",
    Completion => "completion",
    MissingDirectory => "missing_directory",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDetails {
    pub kind: EventKind,
    pub workspace_id: String,
    pub repository_id: String,
    pub name: String,
    pub path: PathBuf,
    /// Observed branch for branch changes; null for a detached HEAD.
    pub branch: Option<String>,
    pub cause: Option<EventCause>,
    pub error: Option<String>,
    /// The mark a review event describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEvent {
    /// The linked issue or PR; both are null for a workspace mark.
    pub kind: Option<crate::forge::link::ItemKind>,
    pub url: Option<String>,
    pub head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEvent {
    pub id: i64,
    pub created_at: i64,
    #[serde(flatten)]
    pub details: EventDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventItem {
    Event(WorkspaceEvent),
    Gap {
        since: i64,
        oldest_id: i64,
        latest_id: i64,
    },
}

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

    pub(crate) async fn latest_workspace_event_id(&self) -> Result<i64> {
        self.store
            .run(|db| {
                Ok(db.query_row(
                    "SELECT COALESCE(MAX(id),0) FROM workspace_events",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// A consistent replay batch, including an explicit expired or future cursor.
    pub async fn workspace_events(&self, since: Option<i64>, limit: u32) -> Result<Vec<EventItem>> {
        ensure!(
            since.is_none_or(|id| id >= 0),
            "event cursor must be nonnegative"
        );
        self.store.run(move |db| {
            let (oldest, latest): (Option<i64>, Option<i64>) = db.query_row(
                "SELECT MIN(id),MAX(id) FROM workspace_events", [], |r| Ok((r.get(0)?,r.get(1)?))
            )?;
            let latest = latest.unwrap_or(0);
            let oldest = oldest.unwrap_or(1);
            let cursor = since.unwrap_or(oldest - 1);
            let mut items = Vec::new();
            let cursor = if cursor < oldest - 1 || cursor > latest {
                items.push(EventItem::Gap { since: cursor, oldest_id: oldest, latest_id: latest });
                oldest - 1
            } else { cursor };
            let mut stmt = db.prepare("SELECT id,created_at,record FROM workspace_events WHERE id>?1 ORDER BY id LIMIT ?2")?;
            for row in stmt.query_map(params![cursor,limit], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))? {
                let (id,created_at,record) = row?;
                items.push(EventItem::Event(WorkspaceEvent { id,created_at,details:serde_json::from_str(&record)? }));
            }
            Ok(items)
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

#[cfg(test)]
mod tests;
