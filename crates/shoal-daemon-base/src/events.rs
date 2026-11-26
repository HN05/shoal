//! The journal of events, which `docs/design/daemon.md` describes.

use crate::model::EventCause;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::{log, store::Store};
use crate::time::UnixTime;
pub use shoal_protocol::events::{
    AgentEvent, AlertEvent, ApprovalEvent, EventCursor, EventDetails, EventItem, EventKind,
    FileEvent, FileEventDetails, JobEndedEvent, ShoalEvent, ShoalEventDetails, ShoalEventKind,
    SpaceEvent, StoppedProcess,
};

/// A journal row as stored: file events carry the space's identity in
/// place of its fields, and Shoal's own events no space.
#[derive(Deserialize)]
#[serde(untagged)]
enum Record {
    File(Box<FileEventDetails>),
    Shoal(Box<ShoalEventDetails>),
    Workspace(Box<EventDetails>),
}

/// The stored event `record` with its journal ID and time.
fn decode(id: EventCursor, created_at: UnixTime, record: &str) -> Result<EventItem> {
    Ok(match serde_json::from_str(record)? {
        Record::Workspace(details) => EventItem::Event(Box::new(SpaceEvent {
            id,
            created_at,
            details: *details,
        })),
        Record::File(details) => EventItem::File(Box::new(FileEvent {
            id,
            created_at,
            details: *details,
        })),
        Record::Shoal(details) => EventItem::Shoal(Box::new(ShoalEvent {
            id,
            created_at,
            details: *details,
        })),
    })
}

/// The log entry of the journal's `item`; a gap is none.
fn log_entry(item: EventItem) -> Option<shoal_protocol::log::LogEntry> {
    use shoal_protocol::log::LogEntry;
    Some(match item {
        EventItem::Event(event) => LogEntry::Event(event),
        EventItem::File(event) => LogEntry::File(event),
        EventItem::Shoal(event) => LogEntry::Shoal(event),
        EventItem::Gap { .. } => return None,
    })
}

/// The events after `cursor`, after a gap where the journal lost some of
/// them.
fn replay(
    db: &Connection,
    cursor: Option<EventCursor>,
    known_pruned: Option<EventCursor>,
    limit: u32,
) -> Result<(Vec<EventItem>, EventCursor)> {
    let bounds = Bounds::read(db)?;
    let (resume, gap) = bounds.resume_after(cursor, known_pruned);
    let mut items: Vec<_> = gap.into_iter().collect();
    items.extend(events_after(db, resume, limit)?);
    Ok((items, bounds.pruned))
}

/// What the journal holds: its oldest and newest IDs, and the highest ID
/// retention deleted.
struct Bounds {
    oldest: EventCursor,
    latest: EventCursor,
    pruned: EventCursor,
}

impl Bounds {
    fn read(db: &Connection) -> Result<Self> {
        let (oldest, latest, pruned): (Option<EventCursor>, Option<EventCursor>, EventCursor) = db
            .query_row(
                "SELECT MIN(id),MAX(id),(SELECT id FROM journal_pruned) FROM space_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        Ok(Self {
            oldest: oldest.unwrap_or(EventCursor::new(1)),
            // Deleting the newest event does not move a follower's cursor ahead.
            latest: latest.unwrap_or(EventCursor::new(0)).max(pruned),
            pruned,
        })
    }

    /// Where a replay from `cursor` goes on, and the gap to report first:
    /// for a cursor outside the journal, or one retention deleted events
    /// after. Per-space retention deletes events anywhere, so for the
    /// client's own cursor (`known_pruned` none) any deletion above it is a
    /// gap, and for a later batch only one since the batch before.
    fn resume_after(
        &self,
        cursor: Option<EventCursor>,
        known_pruned: Option<EventCursor>,
    ) -> (EventCursor, Option<EventItem>) {
        let Some(cursor) = cursor else {
            return (self.oldest.before(), None);
        };
        let outside = cursor < self.oldest.before() || cursor > self.latest;
        let deleted = self.pruned > known_pruned.map_or(cursor, |known| cursor.max(known));
        let resume = if outside {
            self.oldest.before()
        } else {
            cursor
        };
        let gap = (outside || deleted).then_some(EventItem::Gap {
            after: cursor,
            oldest_id: resume.next(),
            latest_id: self.latest,
        });
        (resume, gap)
    }
}

/// The first `limit` events after `cursor`.
fn events_after(db: &Connection, cursor: EventCursor, limit: u32) -> Result<Vec<EventItem>> {
    let mut statement = db.prepare(
        "SELECT id,created_at,record FROM space_events WHERE id>?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = statement.query_map(params![cursor, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
    })?;
    rows.map(|row| {
        let (id, created_at, record) = row?;
        decode(id, created_at, &record)
    })
    .collect()
}

/// Append in the transaction that owns the transition, before deleting ownership.
pub fn record(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    cause: EventCause,
    error: Option<&str>,
) -> Result<()> {
    // Repeated cleanup failures are unchanged state, even when notifications are read.
    db.execute(
        "INSERT INTO space_events(space_id,record)
             SELECT id, json_set(record,'$.kind',?2,'$.cause',?3,'$.error',?4)
             FROM space_event_records WHERE id=?1 AND NOT EXISTS (
                 SELECT 1 FROM space_events WHERE id=(
                     SELECT MAX(id) FROM space_events WHERE space_id=?1
                 ) AND json_extract(record,'$.kind')=?2 AND json_extract(record,'$.cause')=?3
                     AND json_extract(record,'$.error') IS ?4
             )",
        params![id, kind, cause, error],
    )?;
    Ok(())
}

/// Leave the workspace `id`, whose removal for `cause` failed with `error`,
/// in `state`, recording that it stays.
pub fn retain(
    db: &Connection,
    id: &SpaceId,
    state: crate::state::WorkspaceState,
    cause: EventCause,
    error: &str,
) -> Result<()> {
    db.execute(
        "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
        params![id, state, error],
    )?;
    record(db, id, EventKind::Retained, cause, Some(error))
}

/// Record a link mutation in the transaction that changes its association.
pub fn record_link(
    db: &Connection,
    id: &SpaceId,
    event_kind: EventKind,
    relation: crate::model::Relation,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
) -> Result<()> {
    let kind = kind.into();
    ensure!(matches!(
        event_kind,
        EventKind::Linked | EventKind::Unlinked
    ));
    db.execute(
        "INSERT INTO space_events(space_id,record)
             SELECT id, json_set(record,'$.kind',?2,
                 '$.link',json_object('kind',?3,'url',?4,'relation',?5))
             FROM space_event_records WHERE id=?1",
        params![id, event_kind, kind, url, relation],
    )?;
    Ok(())
}

/// Record a change Shoal made to one of the space's issues or PRs.
pub fn record_item(
    db: &Connection,
    id: &SpaceId,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
    action: crate::forge::items::change::ItemChange,
) -> Result<()> {
    let kind = kind.into();
    db.execute(
        "INSERT INTO space_events(space_id,record)
             SELECT id, json_set(record,'$.kind',?2,
                 '$.item',json_object('kind',?3,'url',?4,'action',?5))
             FROM space_event_records WHERE id=?1",
        params![id, EventKind::ItemChanged, kind, url, action],
    )?;
    Ok(())
}

/// Record activity observed on an issue or PR by the background poller.
pub fn record_item_activity(
    db: &Connection,
    id: &SpaceId,
    kind: crate::forge::items::link::LinkKind,
    url: &str,
    update: &crate::forge::updates::Update,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(space_id,record)
             SELECT id, json_set(record,'$.kind',?2,
                 '$.item_activity',json_object('kind',?3,'url',?4,
                     'update',json_object('kind',?5,'message',?6)))
             FROM space_event_records WHERE id=?1",
        params![
            id,
            EventKind::ItemActivity,
            kind,
            url,
            update.kind,
            update.message
        ],
    )?;
    Ok(())
}

/// Record `kind` for the space `id`, with `payload` as its field `field`.
fn record_with(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    field: &str,
    payload: &impl serde::Serialize,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(space_id,record)
             SELECT id, json_set(record,'$.kind',?2,'$.'||?3,json(?4))
             FROM space_event_records WHERE id=?1",
        params![id, kind, field, serde_json::to_string(payload)?],
    )?;
    Ok(())
}

/// Record that no CI run of the default branch started for `merge`, which
/// the space `id` watched, while Shoal looked for one.
pub fn record_no_merge_runs(
    db: &Connection,
    id: &SpaceId,
    merge: &shoal_protocol::events::MergeEvent,
) -> Result<()> {
    record_with(db, id, EventKind::NoMergeRuns, "merge", merge)
}

/// Record the new `progress` of the space `id`.
pub fn record_progress(
    db: &Connection,
    id: &SpaceId,
    progress: &shoal_protocol::progress::Progress,
) -> Result<()> {
    record_with(db, id, EventKind::ProgressChanged, "progress", progress)
}

/// Record that a blocker of the space `id` was added, removed or cleared.
pub fn record_blocker(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    blocker: &shoal_protocol::events::BlockerEvent,
) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::BlockerAdded | EventKind::BlockerRemoved | EventKind::BlockerCleared
    ));
    record_with(db, id, kind, "blocker", blocker)
}

/// Record one side of an absorb for the space `id`: that it was absorbed,
/// or that it absorbed another.
pub fn record_absorb(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    absorb: &shoal_protocol::events::AbsorbEvent,
) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::Absorbed | EventKind::AbsorbedSpace
    ));
    record_with(db, id, kind, "absorb", absorb)
}

/// Record that the agent of the space `id` started, stopped or exited. An
/// execution's agent ends once: a stop that saved it for a restore, then its
/// wrapper's end, records one stop.
fn insert_agent(db: &Connection, id: &SpaceId, kind: EventKind, agent: &AgentEvent) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::AgentStarted | EventKind::AgentStopped | EventKind::AgentExited
    ));
    if kind != EventKind::AgentStarted && agent_ended(db, id, agent.execution_id.as_ref())? {
        return Ok(());
    }
    record_with(db, id, kind, "agent", agent)
}

/// When the agent of the space `id` last stopped or exited, as its log
/// records; none before it did.
pub fn agent_stopped_at(db: &Connection, id: &SpaceId) -> Result<Option<UnixTime>> {
    Ok(db.query_row(
        "SELECT MAX(created_at) FROM space_events WHERE space_id=?1
             AND json_extract(record,'$.kind') IN (?2,?3)",
        params![id, EventKind::AgentStopped, EventKind::AgentExited],
        |row| row.get(0),
    )?)
}

/// The removal of the space `id`, which no space has any longer, while the
/// journal keeps it.
pub fn removal(db: &Connection, id: &SpaceId) -> Result<Option<EventDetails>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_events
             WHERE json_extract(record,'$.kind')=?2 AND space_id=?1
                 AND space_id NOT IN (SELECT id FROM spaces)
             ORDER BY id DESC LIMIT 1",
            params![id, EventKind::Removed],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record.as_deref().map(serde_json::from_str).transpose()?)
}

/// Whether the space's newest agent event is the end of `execution`'s agent.
fn agent_ended(db: &Connection, id: &SpaceId, execution: Option<&ExecutionId>) -> Result<bool> {
    let newest: Option<(EventKind, Option<ExecutionId>)> = db
        .query_row(
            "SELECT json_extract(record,'$.kind'),json_extract(record,'$.agent.execution_id')
             FROM space_events WHERE space_id=?1
                 AND json_extract(record,'$.kind') IN (?2,?3,?4)
             ORDER BY id DESC LIMIT 1",
            params![
                id,
                EventKind::AgentStarted,
                EventKind::AgentStopped,
                EventKind::AgentExited
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(newest.is_some_and(|(kind, ended)| {
        kind != EventKind::AgentStarted && ended.as_ref() == execution
    }))
}

/// Record `details`, an event of Shoal itself.
fn insert_shoal(db: &Connection, details: &ShoalEventDetails) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(record) VALUES (?1)",
        [serde_json::to_string(details)?],
    )?;
    Ok(())
}

/// Record that the request's space asked for it, or that it was decided,
/// in the transaction that stores it.
pub fn record_approval(
    db: &Connection,
    kind: EventKind,
    request: &shoal_protocol::approvals::ApprovalRequest,
) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::ApprovalRequested | EventKind::ApprovalDecided
    ));
    let approval = ApprovalEvent {
        number: request.number,
        target: request.target.to_string(),
        subject: request.subject(),
        status: request.status,
    };
    record_with(db, &request.space_id, kind, "approval", &approval)
}

/// Record the gate's verdict on the request's space's request, in the
/// transaction that records it.
pub fn record_gate(
    db: &Connection,
    request: &shoal_protocol::approvals::ApprovalRequest,
    gate: &shoal_protocol::events::GateEvent,
) -> Result<()> {
    record_with(db, &request.space_id, EventKind::GateDecided, "gate", gate)
}

/// Record an agent's start, stop or exit in the space `id`'s log; a
/// failure is logged, never returned, as it must not fail the agent's
/// lifecycle.
pub async fn record_agent(store: &Store, id: &SpaceId, kind: EventKind, agent: AgentEvent) {
    let space = id.clone();
    let recorded = store
        .run(move |db| insert_agent(db, &space, kind, &agent))
        .await;
    if let Err(error) = recorded {
        log!("agent event of {id} not recorded: {error:#}");
    }
}

/// Record `details`, an event of Shoal itself, in Shoal's log; a failure
/// is logged, never returned, as it must not fail what Shoal did.
pub async fn record_shoal(store: &Store, details: ShoalEventDetails) {
    let kind = details.kind;
    let recorded = store.run(move |db| insert_shoal(db, &details)).await;
    if let Err(error) = recorded {
        log!("{kind} not recorded: {error:#}");
    }
}

/// Record `kind` in Shoal's log, with `error` when what it records failed.
pub async fn record_shoal_outcome(
    store: &Store,
    kind: ShoalEventKind,
    error: Option<&anyhow::Error>,
) {
    let mut details = ShoalEventDetails::new(kind);
    details.error = error.map(|error| format!("{error:#}"));
    record_shoal(store, details).await;
}

/// Record the alerts `alerts` the space `id` sent with `message`, up to its
/// coordinator or the user or down to the agents below it.
pub async fn record_alert(store: &Store, id: &SpaceId, message: String, alerts: Vec<i64>) {
    let space = id.clone();
    let recorded = store
        .run(move |db| {
            let alert = AlertEvent { message, alerts };
            record_with(db, &space, EventKind::AlertSent, "alert", &alert)
        })
        .await;
    if let Err(error) = recorded {
        log!("alert of {id} not recorded: {error:#}");
    }
}

/// Record that Shoal ended the CI job of the workspace `id`, which outlived
/// its run on the forge.
pub async fn record_job_ended(store: &Store, id: &SpaceId, job: JobEndedEvent) {
    let space = id.clone();
    let recorded = store
        .run(move |db| record_with(db, &space, EventKind::JobEnded, "job", &job))
        .await;
    if let Err(error) = recorded {
        log!("job end of {id} not recorded: {error:#}");
    }
}

/// The ID of the journal's newest event.
pub async fn latest_id(store: &Store) -> Result<EventCursor> {
    store
        .run(|db| {
            Ok(
                db.query_row("SELECT COALESCE(MAX(id),0) FROM space_events", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
}

/// A replay batch after `cursor` and the highest ID retention had deleted,
/// `pruned`, when the previous batch read it; none for the client's own
/// cursor. Returns that ID as of this batch, for the next.
pub async fn page(
    store: &Store,
    cursor: Option<EventCursor>,
    pruned: Option<EventCursor>,
    limit: u32,
) -> Result<(Vec<EventItem>, EventCursor)> {
    ensure!(
        cursor.is_none_or(|cursor| cursor.id() >= 0),
        "event cursor must be nonnegative"
    );
    store.run(move |db| replay(db, cursor, pruned, limit)).await
}

/// Leave the workspace `id`, whose removal for `cause` failed with `error`,
/// in `state`, recording that it stays.
pub async fn retain_workspace(
    store: &Store,
    id: &SpaceId,
    state: crate::state::WorkspaceState,
    cause: EventCause,
    error: &anyhow::Error,
) -> Result<()> {
    let id = id.to_owned();
    let error = format!("{error:#}");
    store
        .write(move |tx| retain(tx, &id, state, cause, &error))
        .await
}

/// Record that a removal of the space `id` for `cause` started, unless it
/// retries one for the same cause that just failed, so a cleanup that fails
/// on every sweep leaves one failure in the log rather than a pair for each
/// attempt.
pub async fn record_removal_start(store: &Store, id: &SpaceId, cause: EventCause) -> Result<()> {
    let id = id.to_owned();
    store
        .write(move |tx| {
            let retry = super::store::exists(
                tx,
                "SELECT 1 FROM space_events WHERE id=(
                     SELECT MAX(id) FROM space_events WHERE space_id=?1
                 ) AND json_extract(record,'$.kind')=?2
                     AND json_extract(record,'$.cause')=?3",
                params![id, EventKind::Retained, cause],
            )?;
            if retry {
                return Ok(());
            }
            record(tx, &id, EventKind::RemovalStarted, cause, None)
        })
        .await
}

/// Record that the space `id`, whose removal for `cause` failed with
/// `error`, stays.
pub async fn record_retained(
    store: &Store,
    id: &SpaceId,
    cause: EventCause,
    error: &anyhow::Error,
) -> Result<()> {
    let id = id.to_owned();
    let error = format!("{error:#}");
    store
        .run(move |db| record(db, &id, EventKind::Retained, cause, Some(&error)))
        .await
}

pub mod retention;
pub mod shoal_log;
pub mod space_log;

#[cfg(test)]
mod tests;
