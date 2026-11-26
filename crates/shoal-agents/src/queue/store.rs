//! The queue's rows.
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;

use super::blockers;
use crate::daemon::{Records, Removals};
use crate::model::QueueEntry;
use crate::model::QueueRequest;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::launches::LaunchSource;
use shoal_protocol::time::UnixTime;

/// A waiting row of the queue.
pub struct QueuedRow {
    pub id: QueueEntryId,
    /// The repository of the space it starts in; none for a chat without
    /// one.
    pub repository: Option<RepositoryId>,
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
    /// The agent it starts.
    pub agent: shoal_protocol::agent::Agent,
}

/// The waiting rows of the queue, in order.
pub fn queued_rows(db: &rusqlite::Connection) -> Result<Vec<QueuedRow>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,kind,execution_id,space_id,env IS NOT NULL,automatic,job,mode,
                agent
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
                agent: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// A launch to record in the queue.
pub struct Launch {
    pub id: QueueEntryId,
    /// The repository of the space it starts in; none for a chat without
    /// one.
    pub repository: Option<RepositoryId>,
    pub request: LaunchRequest,
    pub state: QueueState,
    /// The workspace a workspace's launch starts its agent in; none for a
    /// chat's.
    pub opens: Option<SpaceId>,
    /// The recorded chat a chat's launch starts its agent in; none for a
    /// workspace's, or a chat's its caller records once admitted.
    pub chat: Option<SpaceId>,
    /// The space whose scoped process queued a chat's launch, in whose
    /// scope the launch records the chat.
    pub scope: Option<SpaceId>,
    /// What a launch in a workspace waits for there.
    pub awaits: Awaits,
}

/// What a launch in a workspace waits for before it may start.
#[derive(Debug, Clone, Copy, Default)]
pub enum Awaits {
    /// A retried setup, when the workspace's setup failed or never started.
    #[default]
    FailedSetup,
    /// The preparation of the new workspace a start opened, past its
    /// post-setup hook, whatever state the workspace is in, then starting
    /// at once regardless of the limits when `at_once`.
    Preparation { at_once: bool },
}

/// What a launch asks for, a workspace's or a chat's, as its row records it.
pub struct LaunchRequest {
    pub name: shoal_protocol::space_names::SpaceName,
    pub agent: shoal_protocol::agent::Agent,
    pub mode: Option<crate::config::modes::ModeName>,
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub issue_title: Option<String>,
    pub issue_labels: Vec<String>,
    pub plan: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub now: bool,
    pub blockers: Vec<shoal_protocol::progress::Blocker>,
    pub priority: Option<shoal_schedule::priority::Priority>,
    pub source: Option<LaunchSource>,
}

impl From<QueueRequest> for LaunchRequest {
    fn from(request: QueueRequest) -> Self {
        Self {
            name: request.name,
            agent: request.agent,
            mode: request.mode,
            narrowing: request.narrowing,
            issue_title: request.issue_title,
            issue_labels: request.issue_labels,
            plan: request.plan,
            env: request.env,
            now: request.now,
            blockers: request.blockers,
            priority: None,
            source: request.source,
        }
    }
}

/// End the starting launches that created or opened `workspace` whose setup
/// failed, freeing their slots, whether or not their caller is still there to
/// end them. The launch left waiting for its setup takes their place.
fn end_failed_starts(tx: &rusqlite::Transaction<'_>, workspace: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id)
         AND kind=?2 AND state=?3",
        params![workspace, QueueKind::Launch, QueueState::Starting],
    )?;
    Ok(())
}

/// Whether an agent of `workspace`'s own waits in the queue or starts there.
/// The starting launches whose setup failed there do not count, since the
/// launch left waiting for that setup takes their place. Nor does an asleep
/// agent's waiting restore, which counts as its stopped agent.
pub fn queues_agent_in(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    workspace: &SpaceId,
) -> Result<bool> {
    shoal_store::exists(
        tx,
        "SELECT 1 FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id)
         AND kind IN (?2,?3) AND NOT (?5 AND kind=?2 AND state=?4)
         AND NOT (kind=?3 AND automatic AND state=?6)",
        params![
            workspace,
            QueueKind::Launch,
            QueueKind::Restore,
            QueueState::Starting,
            setup_failed(tx, records, workspace)?,
            QueueState::Queued
        ],
    )
}

/// The restores of an asleep agent of `workspace`, which Shoal queued on
/// its own, that wait in the queue.
pub fn asleep_restores(
    tx: &rusqlite::Transaction<'_>,
    workspace: &SpaceId,
) -> Result<Vec<QueueEntryId>> {
    Ok(tx
        .prepare(
            "SELECT id FROM agent_queue WHERE space_id=?1 AND kind=?2 AND automatic AND state=?3",
        )?
        .query_map(
            params![workspace, QueueKind::Restore, QueueState::Queued],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether a launch waits in the queue or starts in `workspace`, holding
/// its one agent.
pub fn queues_launch_in(tx: &rusqlite::Transaction<'_>, workspace: &SpaceId) -> Result<bool> {
    shoal_store::exists(
        tx,
        "SELECT 1 FROM agent_queue WHERE ?1 IN (space_id,opens_workspace_id) AND kind=?2",
        params![workspace, QueueKind::Launch],
    )
}

/// Whether a queued launch waits to open `workspace`.
pub fn launch_waits_to_open(db: &rusqlite::Connection, workspace: &SpaceId) -> Result<bool> {
    shoal_store::exists(
        db,
        "SELECT 1 FROM agent_queue WHERE opens_workspace_id=?1 AND kind=?2 AND state=?3",
        params![workspace, QueueKind::Launch, QueueState::Queued],
    )
}

/// The agent the newest queue entry of the space `id` starts.
pub fn queued_agent(
    db: &rusqlite::Connection,
    id: &SpaceId,
) -> Result<Option<crate::agent::Agent>> {
    Ok(db
        .query_row(
            "SELECT agent FROM agent_queue WHERE space_id=?1 ORDER BY rowid DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether `workspace`'s setup failed, or never started and runs in no
/// execution, as when the daemon refused it, either of which a retried
/// setup can ready.
fn setup_failed(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    workspace: &SpaceId,
) -> Result<bool> {
    use crate::state::WorkspaceState;
    let state = records
        .space_facts(tx, workspace)?
        .and_then(|space| space.state);
    Ok(match state {
        Some(WorkspaceState::Failed) => true,
        Some(WorkspaceState::Preparing) => !shoal_store::exists(
            tx,
            "SELECT 1 FROM executions WHERE space_id=?1",
            [workspace],
        )?,
        _ => false,
    })
}

/// Record `launch` once [`ensure_recordable`] lets it: it takes the place
/// of the start whose setup failed in a workspace it waits in, and of its
/// space's failed launch.
pub fn insert_launch(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    removals: &dyn Removals,
    launch: &Launch,
) -> Result<()> {
    let waits_for_setup = ensure_recordable(tx, records, removals, launch)?;
    if let (Some(workspace), Awaits::FailedSetup) = (&launch.opens, launch.awaits)
        && waits_for_setup
    {
        end_failed_starts(tx, workspace)?;
    }
    let at_once = matches!(launch.awaits, Awaits::Preparation { at_once: true });
    for space in [&launch.opens, &launch.chat].into_iter().flatten() {
        super::failures::replace(tx, space)?;
    }
    let request = &launch.request;
    let now = UnixTime::now();
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,opens_workspace_id,mode,narrowing,scope_space_id,priority,issue_labels,source,starts_at_once,space_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
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
            shoal_store::narrowing_column(&request.narrowing)?,
            launch.scope,
            request.priority,
            serde_json::to_string(&request.issue_labels)?,
            (request.source.as_ref()).map(serde_json::to_string).transpose()?,
            at_once,
            launch.chat,
        ],
    )?;
    if let Some(repository) = &launch.repository {
        blockers::record(tx, records, &launch.id, repository, &request.blockers)?;
    }
    if let Some(workspace) = &launch.opens
        && waits_for_setup
    {
        blockers::wait_for_setup(tx, &launch.id, workspace)?;
    }
    Ok(())
}

/// Refuse `launch` while its repository queues another launch in the same
/// workspace or is being removed. Names repeat, so they tell no two launches
/// apart. A launch in a workspace needs it ready in the same transaction, or
/// failed in its setup for one that waits its turn. A launch a space queued
/// needs that space present and not being removed: removal reserves the space
/// before its final checks, so the launch either precedes them, which keeps the
/// space, or is refused here. Returns whether it waits for the workspace's
/// setup.
pub fn ensure_recordable(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    removals: &dyn Removals,
    launch: &Launch,
) -> Result<bool> {
    if let Some(repository) = &launch.repository {
        ensure!(
            !shoal_store::repository_removing(tx, repository)?,
            "the repository is being removed"
        );
    }
    let waits_for_setup = match (&launch.opens, launch.awaits) {
        (Some(_), Awaits::Preparation { .. }) => true,
        (Some(workspace), Awaits::FailedSetup) => {
            !launch.request.now && setup_failed(tx, records, workspace)?
        }
        (None, _) => false,
    };
    if let (Some(workspace), Awaits::FailedSetup) = (&launch.opens, launch.awaits)
        && !waits_for_setup
    {
        shoal_store::require_ready(tx, workspace).context("start agents in a ready workspace")?;
    }
    if let Some(space) = &launch.scope {
        (removals.require_taking_work(tx, space))
            .context("the space that queued the launch is being removed")?;
    }
    if let Some(chat) = &launch.chat {
        ensure!(
            !queues_launch_in(tx, chat)?,
            "chat {chat} has an agent waiting in the agent queue"
        );
    }
    // The starts whose setup failed give way to a launch that waits there.
    let replaces_failed_starts = waits_for_setup && matches!(launch.awaits, Awaits::FailedSetup);
    ensure!(
        !shoal_store::exists(
            tx,
            "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND kind=?2
                AND opens_workspace_id=?3 AND NOT (?4 AND state=?5)",
            params![
                launch.repository,
                QueueKind::Launch,
                launch.opens,
                replaces_failed_starts,
                QueueState::Starting
            ],
        )?,
        "{} is already in the agent queue",
        launch.request.name
    );
    Ok(waits_for_setup)
}

/// Record a launch `id`, waiting to open `workspace`, that keeps what
/// [`insert_launch`] recorded for the launch `from`.
pub fn copy_launch_waiting(
    tx: &rusqlite::Transaction<'_>,
    from: &QueueEntryId,
    id: &QueueEntryId,
    workspace: &SpaceId,
) -> Result<()> {
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,opens_workspace_id,mode,narrowing,scope_space_id,priority,issue_labels,source)
         SELECT ?2,kind,repository_id,name,agent,issue_title,plan,env,?3,created_at,?4,mode,narrowing,scope_space_id,priority,issue_labels,source
         FROM agent_queue WHERE id=?1",
        params![from, id, QueueState::Queued, workspace],
    )?;
    Ok(())
}

/// The restore of a stopped agent whose wrapper waits for it.
pub struct WaitingRestore {
    pub space: crate::model::Space,
    pub execution: ExecutionId,
    pub agent: crate::agent::Agent,
}

/// Record `restore` behind the work already waiting, as Shoal's own,
/// returning its entry's ID. One already recorded keeps its place and waits
/// again.
pub fn record_waiting_restore(
    db: &rusqlite::Connection,
    restore: &WaitingRestore,
) -> Result<QueueEntryId> {
    let now = UnixTime::now();
    let space = &restore.space;
    db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,seen_at,space_id,execution_id,automatic)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8,?9,1)
         ON CONFLICT(execution_id) DO UPDATE SET state=excluded.state,seen_at=excluded.seen_at",
        params![
            QueueEntryId::generate(),
            QueueKind::Restore,
            space.repository_id.as_ref(),
            &space.name,
            restore.agent,
            QueueState::Queued,
            now,
            &space.id,
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
pub fn last_seq(db: &rusqlite::Connection) -> Result<i64> {
    let seq: Option<i64> = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='agent_queue'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(seq.unwrap_or(0))
}

pub const ENTRY_COLUMNS: &str = "id,kind,repository_id,name,agent,mode,narrowing,issue_title,state,created_at,space_id,execution_id,opens_workspace_id,base_branch,job,scope_space_id";

pub fn entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueEntry> {
    Ok(QueueEntry {
        id: row.get("id")?,
        kind: row.get("kind")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        agent: row.get("agent")?,
        mode: row.get("mode")?,
        narrowing: shoal_store::json_column(row, "narrowing")?.unwrap_or_default(),
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
        job: shoal_store::json_column(row, "job")?,
        held_by: None,
        rank: None,
        error: None,
    })
}

#[cfg(test)]
mod tests;
