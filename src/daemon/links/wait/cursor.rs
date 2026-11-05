//! An item's record on a space's watch cursor, stored in SQLite: what its
//! watch last saw of the item, and the updates delivered from it that are
//! not acknowledged yet, stored apart so reading them reads none of the
//! item's history.
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::forge::updates::{Update, WatchRecord};

/// The item's record on the workspace's watch cursor, if it has one: what
/// its watch saw of the item, or updates queued from it before any watch did.
pub(super) fn cursor_record(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
) -> Result<Option<WatchRecord>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM pr_activity WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    let pending = pending_updates(db, id, url)?;
    if record.is_none() && pending.is_empty() {
        return Ok(None);
    }
    let mut record: WatchRecord = record
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    record.pending = pending;
    Ok(Some(record))
}

/// Store `record` as the item's record on the workspace's watch cursor.
pub(super) fn save_cursor_record(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
    record: &WatchRecord,
) -> Result<()> {
    db.execute(
        "INSERT INTO pr_activity(space_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
        params![id, url, serde_json::to_string(record)?],
    )?;
    save_pending(db, id, url, &record.pending)
}

/// The updates delivered from the item on the workspace's watch cursor that
/// are not acknowledged yet.
pub(super) fn pending_updates(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
) -> Result<Vec<Update>> {
    let updates: Option<String> = db
        .query_row(
            "SELECT updates FROM pending_updates WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(updates
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default())
}

/// Store `updates` as the pending updates of the item on the workspace's
/// watch cursor.
pub(super) fn save_pending(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
    updates: &[Update],
) -> Result<()> {
    if updates.is_empty() {
        db.execute(
            "DELETE FROM pending_updates WHERE space_id=?1 AND url=?2",
            params![id, url],
        )?;
    } else {
        db.execute(
            "INSERT INTO pending_updates(space_id,url,updates) VALUES (?1,?2,?3)
             ON CONFLICT(space_id,url) DO UPDATE SET updates=excluded.updates",
            params![id, url, serde_json::to_string(updates)?],
        )?;
    }
    Ok(())
}

/// The pending updates of every item on the cursors of the spaces `ids`,
/// with their space and URL.
pub(super) fn all_pending(
    db: &rusqlite::Connection,
    ids: &[String],
) -> Result<Vec<(String, String, Vec<Update>)>> {
    let mut statement = db.prepare("SELECT url, updates FROM pending_updates WHERE space_id=?1")?;
    let mut pending = Vec::new();
    for id in ids {
        let rows = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (url, updates) in rows {
            pending.push((id.clone(), url, serde_json::from_str(&updates)?));
        }
    }
    Ok(pending)
}
