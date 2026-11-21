//! The records of moves in progress: each one's space, destination and
//! phase, written before any of its agents stops, with the package's
//! checkpoints once written, so a daemon restarted during a move finishes
//! it from its record.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::daemon::store;
use crate::refusal::{Reason, Refusal};
use crate::time::UnixTime;
use crate::transfer::{MovePhase, MoveRecord};
use shoal_ids::space_ids::SpaceId;

const COLUMNS: &str = "space_id,destination,ticket,carrying,unsaved,phase,checkpoints,saved_before";

/// Record `record`, the move of its space, refusing a space another move
/// already takes.
pub(super) fn insert(tx: &Transaction<'_>, record: &MoveRecord) -> Result<()> {
    if read(tx, &record.space)?.is_some() {
        let message = "another move of this space is recorded; Shoal finishes it first";
        return Err(Refusal::new(Reason::MoveInProgress, message).into());
    }
    tx.execute(
        "INSERT INTO space_moves(space_id,destination,ticket,carrying,unsaved,phase,checkpoints,
            saved_before,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            record.space,
            serde_json::to_string(&record.destination)?,
            record.ticket,
            serde_json::to_string(&record.carrying)?,
            serde_json::to_string(&record.unsaved)?,
            record.phase,
            serde_json::to_string(&record.checkpoints)?,
            serde_json::to_string(&record.saved_before)?,
            UnixTime::now(),
        ],
    )?;
    Ok(())
}

/// Record that the move of `record`'s space reached its phase, with the
/// checkpoints its package holds.
pub(super) fn advance(tx: &Transaction<'_>, record: &MoveRecord) -> Result<()> {
    let changed = tx.execute(
        "UPDATE space_moves SET phase=?2,checkpoints=?3 WHERE space_id=?1",
        params![
            record.space,
            record.phase,
            serde_json::to_string(&record.checkpoints)?
        ],
    )?;
    anyhow::ensure!(changed == 1, "the move of this space ended meanwhile");
    Ok(())
}

/// The move of the space `space`, while one is recorded.
pub(super) fn read(db: &Connection, space: &SpaceId) -> Result<Option<MoveRecord>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM space_moves WHERE space_id=?1"),
            [space],
            record,
        )
        .optional()?)
}

/// Every recorded move.
pub(crate) fn all(db: &Transaction<'_>) -> Result<Vec<MoveRecord>> {
    Ok(db
        .prepare(&format!("SELECT {COLUMNS} FROM space_moves"))?
        .query_map([], record)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces whose move holds its package for the joined machine that
/// started it, which waits for that machine after `now`.
pub(super) fn held_after(db: &Connection, now: UnixTime) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare(
            "SELECT space_id FROM space_moves
                WHERE json_extract(destination,'$.kind')='held' AND held_until>?1",
        )?
        .query_map([now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record that the machine the move of the space `space` holds its
/// package for read all of it, unless that machine ended the space
/// already, and have the move wait for it until `until`.
pub(super) fn read_whole(tx: &Transaction<'_>, space: &SpaceId, until: UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE space_moves SET phase=iif(phase='packing','delivering',phase),held_until=?2
            WHERE space_id=?1",
        params![space, until],
    )?;
    Ok(())
}

/// Have the move of the space `space` wait for the machine that started it
/// until `until`.
pub(super) fn hold_until(tx: &Transaction<'_>, space: &SpaceId, until: UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE space_moves SET held_until=?2 WHERE space_id=?1",
        params![space, until],
    )?;
    Ok(())
}

/// Forget the move of the space `space`.
pub(super) fn forget(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_moves WHERE space_id=?1", [space])?;
    Ok(())
}

fn record(row: &Row<'_>) -> rusqlite::Result<MoveRecord> {
    Ok(MoveRecord {
        space: row.get("space_id")?,
        destination: required(row, "destination")?,
        ticket: row.get("ticket")?,
        carrying: required(row, "carrying")?,
        unsaved: required(row, "unsaved")?,
        phase: row.get::<_, MovePhase>("phase")?,
        checkpoints: required(row, "checkpoints")?,
        saved_before: required(row, "saved_before")?,
    })
}

/// The JSON column `name`, which is never `NULL`.
fn required<T: serde::de::DeserializeOwned>(row: &Row<'_>, name: &str) -> rusqlite::Result<T> {
    store::json_column(row, name)?
        .ok_or_else(|| rusqlite::Error::InvalidColumnName(name.to_owned()))
}
