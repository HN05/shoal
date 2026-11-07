//! The queue's rows: reading the waiting ones and entries, and recording
//! launches and the restores waiting wrappers carry out.
use anyhow::{Context, Result, ensure};
use rusqlite::params;

use super::dependencies;
use crate::client::time::UnixTime;
use crate::{
    model::{QueueEntry, QueueRequest},
    state::{QueueKind, QueueState},
};

/// A waiting row of the queue.
pub(super) struct QueuedRow {
    pub id: String,
    pub repository: String,
    pub kind: QueueKind,
    pub execution: Option<String>,
    pub workspace: Option<String>,
    /// Whether the daemon starts it, rather than a waiting wrapper.
    pub started_here: bool,
    /// Whether Shoal queued the restore on its own.
    pub automatic: bool,
    /// A job's entry's job, as JSON.
    pub job: Option<String>,
}

/// The waiting rows of the queue, in order.
pub(super) fn queued_rows(db: &rusqlite::Connection) -> Result<Vec<QueuedRow>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,kind,execution_id,workspace_id,env IS NOT NULL,automatic,job
             FROM agent_queue WHERE state=?1 ORDER BY seq",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok(QueuedRow {
                id: row.get(0)?,
                repository: row.get(1)?,
                kind: row.get(2)?,
                execution: row.get(3)?,
                workspace: row.get(4)?,
                started_here: row.get(5)?,
                automatic: row.get(6)?,
                job: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// A launch to record in the queue.
pub(super) struct Launch {
    pub id: String,
    pub repository: String,
    pub request: QueueRequest,
    pub state: QueueState,
    /// The existing workspace the launch opens.
    pub opens: Option<String>,
}

/// Record `launch` unless its repository already queues the same name or is
/// being removed. A launch in an existing workspace needs it ready in the same
/// transaction: removal reserves the workspace before its final checks, so the
/// launch either precedes them, which keeps the workspace, or is refused here.
pub(super) fn insert_launch(tx: &rusqlite::Transaction<'_>, launch: &Launch) -> Result<()> {
    ensure!(
        !crate::daemon::store::repository_removing(tx, &launch.repository)?,
        "the repository is being removed"
    );
    if let Some(workspace) = &launch.opens {
        crate::daemon::store::require_ready(tx, workspace)
            .context("start agents in a ready workspace")?;
    }
    let request = &launch.request;
    ensure!(
        !crate::daemon::store::exists(
            tx,
            "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND name=?2 AND kind=?3",
            params![launch.repository, request.name, QueueKind::Launch],
        )?,
        "{} is already in the agent queue",
        request.name
    );
    let now = UnixTime::now();
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,opens_workspace_id,mode)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            launch.id,
            QueueKind::Launch,
            launch.repository,
            request.name,
            request.agent,
            request.issue_title,
            request.plan,
            serde_json::to_string(&request.env)?,
            launch.state,
            now,
            (launch.state == QueueState::Starting).then_some(now),
            launch.opens,
            request.mode,
        ],
    )?;
    dependencies::record(tx, &launch.id, &launch.repository, &request.after)?;
    Ok(())
}

/// The restore of a stopped agent whose wrapper waits for it.
pub(super) struct WaitingRestore {
    pub workspace: crate::model::Workspace,
    pub execution: String,
    pub agent: String,
}

/// Record `restore` behind the work already waiting, returning its entry's
/// ID. One already recorded keeps its place and waits again.
pub(super) fn record_waiting_restore(
    db: &rusqlite::Connection,
    restore: &WaitingRestore,
) -> Result<String> {
    let now = UnixTime::now();
    let workspace = &restore.workspace;
    db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,seen_at,workspace_id,execution_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8,?9)
         ON CONFLICT(execution_id) DO UPDATE SET state=excluded.state,seen_at=excluded.seen_at",
        params![
            uuid::Uuid::new_v4().to_string(),
            QueueKind::Restore,
            workspace.repository_id,
            workspace.name,
            restore.agent,
            QueueState::Queued,
            now,
            workspace.id,
            restore.execution,
        ],
    )?;
    Ok(db.query_row(
        "SELECT id FROM agent_queue WHERE execution_id=?1",
        [&restore.execution],
        |row| row.get(0),
    )?)
}

pub(super) const ENTRY_COLUMNS: &str = "id,kind,repository_id,name,agent,mode,issue_title,state,created_at,workspace_id,execution_id,opens_workspace_id,base_branch,job";

pub(super) fn entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueEntry> {
    Ok(QueueEntry {
        id: row.get("id")?,
        kind: row.get("kind")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        agent: row.get("agent")?,
        mode: row.get("mode")?,
        issue_title: row.get("issue_title")?,
        state: row.get("state")?,
        position: None,
        created_at: row.get("created_at")?,
        workspace_id: row.get("workspace_id")?,
        execution: row.get("execution_id")?,
        opens: row.get("opens_workspace_id")?,
        waiting_for: Vec::new(),
        waits_for_update: false,
        base: row.get("base_branch")?,
        job: crate::daemon::store::json_column(row, "job")?,
    })
}
