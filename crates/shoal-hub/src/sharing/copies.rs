//! The hub's copies of the shared items of a kind: the last edit of each
//! that reached it, under a revision above every earlier copy's.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};

use shoal_protocol::hub::sharing;

use super::{Copy, Edit, Kind, values};

/// Keep each of `edits` that changes an item's copy as its copy now,
/// returning the copies it changed.
pub(crate) fn record<S: Kind>(tx: &Transaction<'_>, edits: Vec<Edit<S>>) -> Result<Vec<Copy<S>>> {
    let mut changed = Vec::new();
    for sharing::Edit { key, value } in edits {
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT value FROM hub_shared_items WHERE kind=?1 AND key=?2",
                params![S::NAME, key.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let current = current
            .map(|current| values::decode::<S>(current))
            .transpose()?;
        if current.is_some_and(|current| current == value) {
            continue;
        }
        let revision: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM hub_shared_items",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO hub_shared_items(kind, key, value, revision)
                VALUES (?1,?2,?3,?4)",
            params![
                S::NAME,
                key.to_string(),
                values::encode::<S>(value.as_ref())?,
                revision
            ],
        )?;
        changed.push(sharing::Copy {
            key,
            value,
            revision,
        });
    }
    Ok(changed)
}

/// The hub's copy of every shared item of `S`, a removed one's without a
/// value.
pub(crate) fn list<S: Kind>(tx: &Transaction<'_>) -> Result<Vec<Copy<S>>> {
    let mut statement =
        tx.prepare("SELECT key, value, revision FROM hub_shared_items WHERE kind=?1 ORDER BY key")?;
    let rows = statement.query_map([S::NAME], values::row)?;
    let mut copies = Vec::new();
    for row in rows {
        copies.push(values::copy::<S>(row?)?);
    }
    Ok(copies)
}
