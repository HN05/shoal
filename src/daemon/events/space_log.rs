//! A space's log, read from the journal: a space that exists by any
//! selector, a removed one by its last name or ID until its events expire,
//! within the caller's scope.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::{EventItem, EventKind, decode};
use crate::{
    client::{
        space_names::SpaceSelector,
        work::{LogEntry, LogFilter},
    },
    daemon::{scope::Caller, workspace::Manager},
    refusal::{Reason, Refusal},
};

/// How far a removed space's log looks up for a space that still exists,
/// through the spaces removed above it.
const REMOVED_ANCESTORS: usize = 32;

impl Manager {
    /// The newest `limit` events of the log of the space `selector` names
    /// that `filter` shows, oldest first.
    pub(crate) async fn space_log(
        &self,
        selector: &SpaceSelector,
        filter: LogFilter,
        limit: u32,
    ) -> Result<Vec<LogEntry>> {
        let id = self.log_space(selector).await?;
        let events = self.store.run(move |db| events_of(db, &id)).await?;
        Ok(shown(events, filter, limit))
    }

    /// Point `selector` at the space whose log a scoped caller reads: its
    /// own or one below it, or a removed space that was below it.
    pub(in crate::daemon) async fn narrow_to_log(
        &self,
        caller: &Caller,
        selector: &mut SpaceSelector,
    ) -> Result<()> {
        let id = self.log_space(selector).await?;
        let mut above = id.clone();
        let mut reached = false;
        for _ in 0..REMOVED_ANCESTORS {
            if self.space_by_id(&above).await.is_ok() {
                reached = self.reaches(caller, &above).await?;
                break;
            }
            let removed = above.clone();
            match self
                .store
                .run(move |db| parent_at_removal(db, &removed))
                .await?
            {
                Some(parent) => above = parent,
                None => {
                    reached = self.reach_root(Some(caller)).await?.is_none();
                    break;
                }
            }
        }
        ensure!(
            reached,
            "workspace processes can only read the logs of their own space and those below it"
        );
        *selector = SpaceSelector::id(&id);
        Ok(())
    }

    /// The ID of the space `selector` names: one that exists, else a
    /// removed one whose events the journal keeps.
    async fn log_space(&self, selector: &SpaceSelector) -> Result<String> {
        if let Ok(space) = self.space(selector).await {
            return Ok(space.id().to_owned());
        }
        let name = selector.id_or_name().map(str::to_owned);
        let removed = match name {
            Some(name) => self.store.run(move |db| removed(db, &name)).await?,
            None => None,
        };
        removed.ok_or_else(|| Refusal::new(Reason::NotFound, format!("no space {selector}")).into())
    }
}

/// Every event of the space `id`, oldest first.
fn events_of(db: &Connection, id: &str) -> Result<Vec<LogEntry>> {
    let mut statement = db.prepare(
        "SELECT id,created_at,record FROM workspace_events WHERE space_id=?1 ORDER BY id",
    )?;
    let rows = statement.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let mut entries = Vec::new();
    for row in rows {
        let (id, created_at, record): (i64, i64, String) = row?;
        entries.push(match decode(id, created_at, &record)? {
            EventItem::Event(event) => LogEntry::Event(event),
            EventItem::Chat(event) => LogEntry::Chat(event),
            EventItem::File(event) => LogEntry::File(event),
            EventItem::Gap { .. } => continue,
        });
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

/// The removed space whose last name or ID is `name`, the most recently
/// removed first.
fn removed(db: &Connection, name: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT space_id FROM workspace_events
             WHERE json_extract(record,'$.kind') IN (?2,'chat_ended')
                 AND (space_id=?1 OR COALESCE(json_extract(record,'$.name'),
                     json_extract(record,'$.chat.name'))=?1)
                 AND space_id NOT IN (SELECT id FROM workspaces)
                 AND space_id NOT IN (SELECT id FROM chats)
             ORDER BY id DESC LIMIT 1",
            params![name, EventKind::Removed],
            |row| row.get(0),
        )
        .optional()?)
}

/// The space the removed space `id` was below when it went, as that
/// space's log recorded it.
fn parent_at_removal(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT space_id FROM workspace_events
             WHERE json_extract(record,'$.kind')=?2 AND json_extract(record,'$.child.id')=?1
             ORDER BY id DESC LIMIT 1",
            params![id, EventKind::ChildRemoved],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
