//! The queue's rows: reading the waiting ones and entries, and recording
//! launches and the restores waiting wrappers carry out.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

use super::blockers;
use crate::client::time::UnixTime;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use crate::{
    model::{QueueEntry, QueueRequest},
    state::{QueueKind, QueueState},
};

/// A waiting row of the queue.
pub(super) struct QueuedRow {
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    pub kind: QueueKind,
    pub execution: Option<ExecutionId>,
    pub workspace: Option<SpaceId>,
    /// Whether the daemon starts it, rather than a waiting wrapper.
    pub started_here: bool,
    /// Whether Shoal queued the restore on its own.
    pub automatic: bool,
    /// A job's entry's job, as JSON.
    pub job: Option<String>,
    /// The mode a launch's agent starts in.
    pub mode: Option<crate::config::modes::ModeName>,
    /// When it was queued.
    pub queued_at: UnixTime,
}

/// The waiting rows of the queue, in order.
pub(super) fn queued_rows(db: &rusqlite::Connection) -> Result<Vec<QueuedRow>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,kind,execution_id,space_id,env IS NOT NULL,automatic,job,mode,
                created_at
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
                mode: row.get(8)?,
                queued_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// A launch to record in the queue.
pub(super) struct Launch {
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    pub request: QueueRequest,
    pub state: QueueState,
    /// The existing workspace the launch opens.
    pub opens: Option<SpaceId>,
    /// The space whose scoped process queued the launch of a new
    /// workspace, in whose scope the launch opens it.
    pub scope: Option<SpaceId>,
}

/// End the starting launches that created or opened `workspace`, whose
/// setup failed, freeing their slots: the launch left waiting for its setup
/// takes their place, whether or not their caller is still there to end
/// them.
fn end_failed_starts(tx: &rusqlite::Transaction<'_>, workspace: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id)
         AND kind=?2 AND state=?3",
        params![workspace, QueueKind::Launch, QueueState::Starting],
    )?;
    Ok(())
}

/// Whether an agent of `workspace`'s own waits in the queue or starts there.
/// The starting launches whose setup failed there do not count: the launch
/// left waiting for that setup takes their place, and any other is refused
/// until the workspace is ready.
pub(super) fn queues_agent_in(tx: &rusqlite::Transaction<'_>, workspace: &SpaceId) -> Result<bool> {
    crate::daemon::store::exists(
        tx,
        "SELECT 1 FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id)
         AND kind IN (?2,?3) AND NOT (?5 AND kind=?2 AND state=?4)",
        params![
            workspace,
            QueueKind::Launch,
            QueueKind::Restore,
            QueueState::Starting,
            setup_failed(tx, workspace)?
        ],
    )
}

/// Whether a launch waits in the queue or starts in `workspace`, holding
/// its one agent.
pub(super) fn queues_launch_in(
    tx: &rusqlite::Transaction<'_>,
    workspace: &SpaceId,
) -> Result<bool> {
    crate::daemon::store::exists(
        tx,
        "SELECT 1 FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id) AND kind=?2",
        params![workspace, QueueKind::Launch],
    )
}

/// Whether `workspace`'s setup failed, or never started and runs in no
/// execution, as when the daemon refused it, either of which a retried
/// setup can ready.
fn setup_failed(tx: &rusqlite::Transaction<'_>, workspace: &SpaceId) -> Result<bool> {
    crate::daemon::store::exists(
        tx,
        "SELECT 1 FROM workspaces w WHERE id=?1 AND (state=?2 OR state=?3
            AND NOT EXISTS (SELECT 1 FROM executions e WHERE e.space_id=w.id))",
        params![
            workspace,
            crate::state::WorkspaceState::Failed,
            crate::state::WorkspaceState::Preparing
        ],
    )
}

/// Whether the launch `plan` opens an existing branch, whose workspace it
/// reopens should the branch have one by then.
pub(super) fn opens_existing_branch(plan: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(plan)
        .is_ok_and(|plan| plan["creation"]["existing"].is_string())
}

/// The branch the launch `plan` creates or opens, if it names one.
fn planned_branch(plan: &str) -> Option<String> {
    let plan: serde_json::Value = serde_json::from_str(plan).ok()?;
    let creation = &plan["creation"];
    let branch = creation["branch"]
        .as_str()
        .or(creation["existing"].as_str())?;
    Some(branch.to_owned())
}

/// Record `launch` unless its repository already queues a launch in the
/// same workspace, or of the same branch, or is being removed; names repeat,
/// so they tell no two launches apart. A launch in an existing workspace
/// needs it ready in the same transaction, or failed in its setup for one
/// that waits its turn, which then waits for a preparation there to get
/// past its post-setup hook and takes the place of the start whose setup
/// failed, and one a space queued needs that space present and not being
/// removed: removal reserves the space before its final checks, so the
/// launch either precedes them, which keeps the space, or is refused here.
pub(super) fn insert_launch(
    tx: &rusqlite::Transaction<'_>,
    launch: &Launch,
    removing: &crate::daemon::chats::Removing,
) -> Result<()> {
    ensure!(
        !crate::daemon::store::repository_removing(tx, &launch.repository)?,
        "the repository is being removed"
    );
    let waits_for_setup = match &launch.opens {
        Some(workspace) => !launch.request.now && setup_failed(tx, workspace)?,
        None => false,
    };
    if let Some(workspace) = &launch.opens {
        if waits_for_setup {
            end_failed_starts(tx, workspace)?;
        } else {
            crate::daemon::store::require_ready(tx, workspace)
                .context("start agents in a ready workspace")?;
        }
    }
    if let Some(space) = &launch.scope {
        require_queuing_space(tx, space, removing)
            .context("the space that queued the launch is being removed")?;
    }
    let request = &launch.request;
    ensure!(
        !crate::daemon::store::exists(
            tx,
            "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND kind=?2 AND CASE
                WHEN ?3 IS NOT NULL THEN opens_workspace_id IS ?3
                WHEN ?4 IS NOT NULL AND opens_workspace_id IS NULL AND json_valid(plan)
                    THEN coalesce(json_extract(plan,'$.creation.branch'),
                        json_extract(plan,'$.creation.existing')) IS ?4
                ELSE 0 END",
            params![
                launch.repository,
                QueueKind::Launch,
                launch.opens,
                planned_branch(&request.plan)
            ],
        )?,
        "{} is already in the agent queue",
        request.name
    );
    let now = UnixTime::now();
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,opens_workspace_id,mode,narrowing,scope_space_id,priority)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
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
            crate::daemon::store::narrowing_column(&request.narrowing)?,
            launch.scope,
            request.priority,
        ],
    )?;
    blockers::record(tx, &launch.id, &launch.repository, &request.blockers)?;
    if let Some(workspace) = &launch.opens
        && waits_for_setup
    {
        blockers::wait_for_setup(tx, &launch.id, workspace)?;
    }
    Ok(())
}

/// Refuse the space `id` a launch's scope names once it is gone, or while a
/// removal holds it: a workspace that is not ready, or a chat `removing`
/// holds.
fn require_queuing_space(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    removing: &crate::daemon::chats::Removing,
) -> Result<()> {
    if crate::daemon::links::space::is_chat(tx, id)? {
        ensure!(!removing.reserved(id), "a removal holds the chat");
        return Ok(());
    }
    crate::daemon::store::require_ready(tx, id)
}

/// The restore of a stopped agent whose wrapper waits for it.
pub(super) struct WaitingRestore {
    pub workspace: crate::model::Workspace,
    pub execution: ExecutionId,
    pub agent: crate::agent::Agent,
}

/// Record `restore` behind the work already waiting, as Shoal's own,
/// returning its entry's ID. One already recorded keeps its place and waits
/// again.
pub(super) fn record_waiting_restore(
    db: &rusqlite::Connection,
    restore: &WaitingRestore,
) -> Result<QueueEntryId> {
    let now = UnixTime::now();
    let workspace = &restore.workspace;
    db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,seen_at,space_id,execution_id,automatic)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8,?9,1)
         ON CONFLICT(execution_id) DO UPDATE SET state=excluded.state,seen_at=excluded.seen_at",
        params![
            QueueEntryId::generate(),
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

/// The sequence number of the newest queue entry, even one that has left:
/// every entry queued later has a higher one, and it never falls.
pub(crate) fn last_seq(db: &rusqlite::Connection) -> Result<i64> {
    let seq: Option<i64> = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='agent_queue'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(seq.unwrap_or(0))
}

pub(super) const ENTRY_COLUMNS: &str = "id,kind,repository_id,name,agent,mode,narrowing,issue_title,state,created_at,space_id,execution_id,opens_workspace_id,base_branch,job,scope_space_id";

pub(super) fn entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueEntry> {
    Ok(QueueEntry {
        id: row.get("id")?,
        kind: row.get("kind")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        agent: row.get("agent")?,
        mode: row.get("mode")?,
        narrowing: crate::daemon::store::json_column(row, "narrowing")?.unwrap_or_default(),
        issue_title: row.get("issue_title")?,
        state: row.get("state")?,
        position: None,
        created_at: row.get("created_at")?,
        space_id: row.get("space_id")?,
        execution: row.get("execution_id")?,
        opens: row.get("opens_workspace_id")?,
        queued_by: row.get("scope_space_id")?,
        blockers: Vec::new(),
        waits_for_update: false,
        base: row.get("base_branch")?,
        job: crate::daemon::store::json_column(row, "job")?,
        held_by: None,
        rank: None,
    })
}
