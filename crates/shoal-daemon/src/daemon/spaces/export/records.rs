//! The records of moves in progress: each one's space, destination and
//! phase, written before any of its agents stops, with the package's
//! checkpoints once written, so a daemon restarted during a move finishes
//! it from its record.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::daemon::store;
use crate::time::UnixTime;
use crate::transfer::{MovePhase, MoveRecord};
use shoal_ids::space_ids::SpaceId;

const COLUMNS: &str = "space_id,destination,carrying,unsaved,phase,checkpoints,saved_before";

/// Record `record`, the move of its space, refusing a space another move
/// already takes.
pub(super) fn insert(tx: &Transaction<'_>, record: &MoveRecord) -> Result<()> {
    anyhow::ensure!(
        read(tx, &record.space)?.is_none(),
        "another move takes this space; wait for it to end"
    );
    tx.execute(
        "INSERT INTO space_moves(space_id,destination,carrying,unsaved,phase,checkpoints,
            saved_before,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            record.space,
            serde_json::to_string(&record.destination)?,
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

/// Forget the move of the space `space`.
pub(super) fn forget(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_moves WHERE space_id=?1", [space])?;
    Ok(())
}

fn record(row: &Row<'_>) -> rusqlite::Result<MoveRecord> {
    Ok(MoveRecord {
        space: row.get("space_id")?,
        destination: required(row, "destination")?,
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
