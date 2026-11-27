//! The journal of events, which `docs/design/daemon.md` describes.

use crate::model::EventCause;
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::{log, store::Store};
use crate::time::UnixTime;
pub use shoal_protocol::events::{
    AgentEvent, AlertEvent, ApprovalEvent, EventCursor, EventDetails, EventItem, EventKind,
    FileEvent, FileEventDetails, JobEndedEvent, JournaledSpaceEvent, ShoalEvent, ShoalEventDetails,
    ShoalEventKind, SpaceEvent, StoppedProcess,
};
use shoal_protocol::events::{
    FileEventKind, ItemActivityEvent, ItemActivityUpdate, ItemEvent, LinkEvent,
};
use shoal_store::journal;

/// The columns [`Stored::read`] reads a journal row by.
const STORED: &str = "id,created_at,kind,record";

/// A journal row as stored. Its kind says which record it holds: file
/// events carry the space's identity in place of its fields, and Shoal's
/// own events no space.
struct Stored {
    id: EventCursor,
    created_at: UnixTime,
    kind: String,
    record: String,
}

impl Stored {
    /// The row's [`STORED`] columns.
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            created_at: row.get(1)?,
            kind: row.get(2)?,
            record: row.get(3)?,
        })
    }

    /// The event the row holds, with its journal ID and time.
    fn decode(self) -> Result<EventItem> {
        let (id, created_at) = (self.id, self.created_at);
        let item = if is_kind::<FileEventKind>(&self.kind) {
            EventItem::File(Box::new(FileEvent {
                id,
                created_at,
                details: self.details()?,
            }))
        } else if is_kind::<ShoalEventKind>(&self.kind) {
            EventItem::Shoal(Box::new(ShoalEvent {
                id,
                created_at,
                details: self.details()?,
            }))
        } else {
            EventItem::Event(Box::new(JournaledSpaceEvent {
                id,
                created_at,
                details: self.details()?,
            }))
        };
        Ok(item)
    }

    fn details<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.record)
            .with_context(|| format!("decode journal event {} of kind {}", self.id, self.kind))
    }
}

/// Whether `kind` names a kind of `T`.
fn is_kind<T: serde::de::DeserializeOwned>(kind: &str) -> bool {
    serde_json::from_value::<T>(kind.into()).is_ok()
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
    let mut statement = db.prepare(&format!(
        "SELECT {STORED} FROM space_events WHERE id>?1 ORDER BY id LIMIT ?2"
    ))?;
    let rows = statement.query_map(params![cursor, limit], Stored::read)?;
    rows.map(|row| row?.decode()).collect()
}

/// Append in the transaction that owns the transition, before deleting ownership.
pub fn record(
    db: &Connection,
    id: &SpaceId,
    event: SpaceEvent,
    cause: EventCause,
    error: Option<&str>,
) -> Result<()> {
    // Repeated cleanup failures are unchanged state, even when notifications are read.
    let repeated = newest_event(db, id)?.is_some_and(|newest| {
        newest.kind() == event.kind()
            && newest.cause == Some(cause)
            && newest.error.as_deref() == error
    });
    if repeated {
        return Ok(());
    }
    journal::append_caused(db, id, event, Some(cause), error)
}

/// The newest event of the space `id`; none when the newest is a file's.
fn newest_event(db: &Connection, id: &SpaceId) -> Result<Option<EventDetails>> {
    let newest = db
        .query_row(
            &format!(
                "SELECT {STORED} FROM space_events WHERE space_id=?1 ORDER BY id DESC LIMIT 1"
            ),
            [id],
            Stored::read,
        )
        .optional()?;
    Ok(match newest.map(Stored::decode).transpose()? {
        Some(EventItem::Event(event)) => Some(event.details),
        _ => None,
    })
}

/// The newest event of the space `id` of one of `kinds`.
pub fn newest_of(
    db: &Connection,
    id: &SpaceId,
    kinds: &[EventKind],
) -> Result<Option<EventDetails>> {
    let listed = vec!["?"; kinds.len()].join(",");
    let parameters = std::iter::once(id.as_str()).chain(kinds.iter().map(|kind| kind.as_str()));
    let newest = db
        .query_row(
            &format!(
                "SELECT {STORED} FROM space_events WHERE space_id=?
                    AND kind IN ({listed}) ORDER BY id DESC LIMIT 1"
            ),
            rusqlite::params_from_iter(parameters),
            Stored::read,
        )
        .optional()?;
    Ok(match newest.map(Stored::decode).transpose()? {
        Some(EventItem::Event(event)) => Some(event.details),
        _ => None,
    })
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
    record(db, id, SpaceEvent::Retained, cause, Some(error))
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
    let link = Box::new(LinkEvent {
        kind: kind.into(),
        url: url.to_owned(),
        relation,
    });
    let event = match event_kind {
        EventKind::Linked => SpaceEvent::Linked { link },
        EventKind::Unlinked => SpaceEvent::Unlinked { link },
        other => bail!("{other} records no link"),
    };
    journal::append(db, id, event)
}

/// Record a change Shoal made to one of the space's issues or PRs.
pub fn record_item(
    db: &Connection,
    id: &SpaceId,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
    action: crate::forge::items::change::ItemChange,
) -> Result<()> {
    let item = Box::new(ItemEvent {
        kind: kind.into(),
        url: url.to_owned(),
        action,
    });
    journal::append(db, id, SpaceEvent::ItemChanged { item })
}

/// Record activity observed on an issue or PR by the background poller.
pub fn record_item_activity(
    db: &Connection,
    id: &SpaceId,
    kind: crate::forge::items::link::LinkKind,
    url: &str,
    update: &crate::forge::updates::Update,
) -> Result<()> {
    let item_activity = Box::new(ItemActivityEvent {
        kind,
        url: url.to_owned(),
        update: ItemActivityUpdate {
            kind: update.kind,
            message: update.message.clone(),
        },
    });
    journal::append(db, id, SpaceEvent::ItemActivity { item_activity })
}

/// Record that no CI run of the default branch started for `merge`, which
/// the space `id` watched, while Shoal looked for one.
pub fn record_no_merge_runs(
    db: &Connection,
    id: &SpaceId,
    merge: &shoal_protocol::events::MergeEvent,
) -> Result<()> {
    let merge = Box::new(merge.clone());
    journal::append(db, id, SpaceEvent::NoMergeRuns { merge })
}

/// Record the new `progress` of the space `id`.
pub fn record_progress(
    db: &Connection,
    id: &SpaceId,
    progress: &shoal_protocol::progress::Progress,
) -> Result<()> {
    let progress = Box::new(progress.clone());
    journal::append(db, id, SpaceEvent::ProgressChanged { progress })
}

/// Record that a blocker of the space `id` was added, removed or cleared.
pub fn record_blocker(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    blocker: &shoal_protocol::events::BlockerEvent,
) -> Result<()> {
    let blocker = Box::new(blocker.clone());
    let event = match kind {
        EventKind::BlockerAdded => SpaceEvent::BlockerAdded { blocker },
        EventKind::BlockerRemoved => SpaceEvent::BlockerRemoved { blocker },
        EventKind::BlockerCleared => SpaceEvent::BlockerCleared { blocker },
        other => bail!("{other} records no blocker"),
    };
    journal::append(db, id, event)
}

/// Record one side of an absorb for the space `id`: that it was absorbed,
/// or that it absorbed another.
pub fn record_absorb(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    absorb: &shoal_protocol::events::AbsorbEvent,
) -> Result<()> {
    let absorb = Box::new(absorb.clone());
    let event = match kind {
        EventKind::Absorbed => SpaceEvent::Absorbed { absorb },
        EventKind::AbsorbedSpace => SpaceEvent::AbsorbedSpace { absorb },
        other => bail!("{other} records no absorb"),
    };
    journal::append(db, id, event)
}

/// Record that the agent of the space `id` started, stopped or exited. An
/// execution's agent ends once: a stop that saved it for a restore, then its
/// wrapper's end, records one stop.
fn insert_agent(db: &Connection, id: &SpaceId, kind: EventKind, agent: AgentEvent) -> Result<()> {
    if kind != EventKind::AgentStarted && agent_ended(db, id, agent.execution_id.as_ref())? {
        return Ok(());
    }
    let agent = Box::new(agent);
    let event = match kind {
        EventKind::AgentStarted => SpaceEvent::AgentStarted { agent },
        EventKind::AgentStopped => SpaceEvent::AgentStopped { agent },
        EventKind::AgentExited => SpaceEvent::AgentExited { agent },
        other => bail!("{other} records no agent"),
    };
    journal::append(db, id, event)
}

/// When the agent of the space `id` last stopped or exited, as its log
/// records; none before it did.
pub fn agent_stopped_at(db: &Connection, id: &SpaceId) -> Result<Option<UnixTime>> {
    Ok(db.query_row(
        "SELECT MAX(created_at) FROM space_events WHERE space_id=?1
             AND kind IN (?2,?3)",
        params![id, EventKind::AgentStopped, EventKind::AgentExited],
        |row| row.get(0),
    )?)
}

/// The removal of the space `id`, which no space has any longer, while the
/// journal keeps it.
pub fn removal(db: &Connection, id: &SpaceId) -> Result<Option<EventDetails>> {
    if super::store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [id])? {
        return Ok(None);
    }
    newest_of(db, id, &[EventKind::Removed])
}

/// Whether the space's newest agent event is the end of `execution`'s agent.
fn agent_ended(db: &Connection, id: &SpaceId, execution: Option<&ExecutionId>) -> Result<bool> {
    let kinds = [
        EventKind::AgentStarted,
        EventKind::AgentStopped,
        EventKind::AgentExited,
    ];
    let newest = newest_of(db, id, &kinds)?;
    Ok(newest.is_some_and(|newest| {
        newest.kind() != EventKind::AgentStarted
            && newest
                .event
                .agent()
                .and_then(|agent| agent.execution_id.as_ref())
                == execution
    }))
}

/// Record that the request's space asked for it, or that it was decided,
/// in the transaction that stores it.
pub fn record_approval(
    db: &Connection,
    kind: EventKind,
    request: &shoal_protocol::approvals::ApprovalRequest,
) -> Result<()> {
    let approval = Box::new(ApprovalEvent {
        number: request.number,
        target: request.target.to_string(),
        subject: request.subject(),
        status: request.status,
    });
    let event = match kind {
        EventKind::ApprovalRequested => SpaceEvent::ApprovalRequested { approval },
        EventKind::ApprovalDecided => SpaceEvent::ApprovalDecided { approval },
        other => bail!("{other} records no approval"),
    };
    journal::append(db, &request.space_id, event)
}

/// Record the gate's verdict on the request's space's request, in the
/// transaction that records it.
pub fn record_gate(
    db: &Connection,
    request: &shoal_protocol::approvals::ApprovalRequest,
    gate: &shoal_protocol::events::GateEvent,
) -> Result<()> {
    let gate = Box::new(gate.clone());
    journal::append(db, &request.space_id, SpaceEvent::GateDecided { gate })
}

/// Record an agent's start, stop or exit in the space `id`'s log, and a
/// stop's cause in the daemon's; a failure is logged, never returned, as it
/// must not fail the agent's lifecycle.
pub async fn record_agent(store: &Store, id: &SpaceId, kind: EventKind, agent: AgentEvent) {
    if kind == EventKind::AgentStopped {
        let cause =
            (agent.cause.as_ref()).map_or_else(|| "unknown".to_owned(), ToString::to_string);
        log!("agent {} of {id} stopped: {cause}", agent.name);
    }
    let space = id.clone();
    let recorded = store
        .run(move |db| insert_agent(db, &space, kind, agent))
        .await;
    if let Err(error) = recorded {
        log!("agent event of {id} not recorded: {error:#}");
    }
}

/// Record `details`, an event of Shoal itself, in Shoal's log; a failure
/// is logged, never returned, as it must not fail what Shoal did.
pub async fn record_shoal(store: &Store, details: ShoalEventDetails) {
    let kind = details.kind;
    let recorded = store
        .run(move |db| journal::append_shoal(db, &details))
        .await;
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
            journal::append(db, &space, SpaceEvent::AlertSent { alert })
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
        .run(move |db| {
            let job = Box::new(job);
            journal::append(db, &space, SpaceEvent::JobEnded { job })
        })
        .await;
    if let Err(error) = recorded {
        log!("job end of {id} not recorded: {error:#}");
    }
}

/// Record that the `shoal run` command of `ran` ended in the space `id`.
pub fn record_command_ran(
    db: &Connection,
    id: &SpaceId,
    ran: &shoal_protocol::events::CommandRanEvent,
) -> Result<()> {
    let command = Box::new(ran.clone());
    journal::append(db, id, SpaceEvent::CommandRan { command })
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
            let retry = newest_event(tx, &id)?.is_some_and(|newest| {
                newest.kind() == EventKind::Retained && newest.cause == Some(cause)
            });
            if retry {
                return Ok(());
            }
            record(tx, &id, SpaceEvent::RemovalStarted, cause, None)
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
        .run(move |db| record(db, &id, SpaceEvent::Retained, cause, Some(&error)))
        .await
}

pub mod agent_time;
pub mod retention;
pub mod shoal_log;
pub mod space_log;

#[cfg(test)]
mod tests;
