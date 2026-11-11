//! Shoal's log, read from the journal: the events of every space, removed
//! ones until their events expire, with Shoal's own.
use anyhow::Result;
use rusqlite::{Transaction, params};

use super::{EventCursor, EventKind, decode, log_entry};
use crate::time::UnixTime;
use crate::{
    client::log::{LogEntry, LogFilter},
    daemon::workspace::Manager,
};

/// How many journal rows a read of Shoal's log takes at a time.
const BATCH: u32 = 500;

impl Manager {
    /// The newest `limit` events of Shoal's log that `filter` shows, oldest
    /// first.
    pub(crate) async fn shoal_log(&self, filter: LogFilter, limit: u32) -> Result<Vec<LogEntry>> {
        self.store
            .read(move |tx| newest_shown(tx, filter, limit))
            .await
    }
}

/// The newest `limit` events that `filter` shows, oldest first, read back
/// from the newest a batch at a time.
fn newest_shown(tx: &Transaction<'_>, filter: LogFilter, limit: u32) -> Result<Vec<LogEntry>> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut shown = Vec::new();
    let mut before = EventCursor::new(i64::MAX);
    while shown.len() < limit {
        let batch = batch_before(tx, before, filter)?;
        let Some(oldest) = batch.last() else {
            break;
        };
        before = oldest.id();
        for entry in batch {
            if shown.len() < limit && shows(tx, &entry, filter)? {
                shown.push(entry);
            }
        }
    }
    shown.reverse();
    Ok(shown)
}

/// Up to [`BATCH`] events before `before`, newest first, leaving out for
/// [`LogFilter::Entries`] the frequent events it never shows.
fn batch_before(
    tx: &Transaction<'_>,
    before: EventCursor,
    filter: LogFilter,
) -> Result<Vec<LogEntry>> {
    let mut statement = tx.prepare(
        "SELECT id,created_at,record FROM workspace_events
         WHERE id<?1 AND (?2 OR NOT bulk) ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = statement.query_map(params![before, filter == LogFilter::All, BATCH], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (id, created_at, record): (EventCursor, UnixTime, String) = row?;
        entries.extend(log_entry(decode(id, created_at, &record)?));
    }
    Ok(entries)
}

/// Whether `filter` shows `entry`. A workspace's creation after its removal
/// is the reopening its restore records, so only every event shows it.
fn shows(tx: &Transaction<'_>, entry: &LogEntry, filter: LogFilter) -> Result<bool> {
    if filter == LogFilter::All {
        return Ok(true);
    }
    let LogEntry::Event(event) = entry else {
        return Ok(entry.is_entry());
    };
    if event.details.kind != EventKind::Created {
        return Ok(entry.is_entry());
    }
    let reopened: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_events
             WHERE space_id=?1 AND id<?2 AND json_extract(record,'$.kind')=?3)",
        params![event.details.space_id(), event.id, EventKind::Removed],
        |row| row.get(0),
    )?;
    Ok(!reopened)
}

#[cfg(test)]
mod tests;
