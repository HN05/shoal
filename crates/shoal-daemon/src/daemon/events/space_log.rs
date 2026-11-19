//! A space's log, read from the journal, which keeps a removed space's
//! events until they expire.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::{EventCursor, EventKind, decode, log_entry};
use crate::daemon::store::Store;
use crate::time::UnixTime;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::log::{LogEntry, LogFilter};

/// The newest `limit` events of the space `id`'s log that `filter` shows,
/// oldest first.
pub(crate) async fn read(
    store: &Store,
    id: SpaceId,
    filter: LogFilter,
    limit: u32,
) -> Result<Vec<LogEntry>> {
    let events = store.run(move |db| events_of(db, &id)).await?;
    Ok(shown(events, filter, limit))
}

/// The removed space `id`, while the journal keeps its removal.
pub(crate) async fn removed(store: &Store, id: SpaceId) -> Result<Option<SpaceId>> {
    store
        .run(move |db| Ok(super::removal(db, &id)?.map(|removal| removal.space_id)))
        .await
}

/// The space the removed space `id` was below when it went, as that
/// space's log recorded it.
pub(crate) async fn parent_at_removal(store: &Store, id: SpaceId) -> Result<Option<SpaceId>> {
    store
        .run(move |db| {
            Ok(db
                .query_row(
                    "SELECT space_id FROM space_events
                     WHERE json_extract(record,'$.kind')=?2
                         AND json_extract(record,'$.child.id')=?1
                     ORDER BY id DESC LIMIT 1",
                    params![id, EventKind::ChildRemoved],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
}

/// Every event of the space `id`, oldest first.
fn events_of(db: &Connection, id: &SpaceId) -> Result<Vec<LogEntry>> {
    let mut statement =
        db.prepare("SELECT id,created_at,record FROM space_events WHERE space_id=?1 ORDER BY id")?;
    let rows = statement.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let mut entries = Vec::new();
    for row in rows {
        let (id, created_at, record): (EventCursor, UnixTime, String) = row?;
        entries.extend(log_entry(decode(id, created_at, &record)?));
    }
    Ok(entries)
}

/// The newest `limit` of `events` that `filter` shows. A workspace's
/// creation after its removal is the reopening its restore records, so
/// only every event shows it.
fn shown(events: Vec<LogEntry>, filter: LogFilter, limit: u32) -> Vec<LogEntry> {
    let mut removed = false;
    let mut shown: Vec<_> = events
        .into_iter()
        .filter(|entry| {
            let kind = match entry {
                LogEntry::Event(event) => Some(event.details.kind),
                _ => None,
            };
            let reopened = removed && kind == Some(EventKind::Created);
            removed |= kind == Some(EventKind::Removed);
            filter == LogFilter::All || (entry.is_entry() && !reopened)
        })
        .collect();
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    shown.drain(..shown.len().saturating_sub(limit));
    shown
}

#[cfg(test)]
mod tests;
