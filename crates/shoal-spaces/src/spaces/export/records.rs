//! The records of moves in progress, written before any agent stops, so a
//! daemon restarted during a move finishes it from its record.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::refusal::{Reason, Refusal};
use crate::store;
use crate::time::UnixTime;
use crate::transfer::{MoveDestination, MovePhase, MoveRecord};
use rusqlite::types::Value as SqlValue;
use shoal_ids::space_ids::SpaceId;
use shoal_store::split::{self, Column};

const COLUMNS: &str =
    "space_id,destination,destination_kind,ticket,carrying,unsaved,phase,checkpoints,saved_before";

/// The column that keeps the destination's kind, which a query reads.
const DESTINATION: &[Column] = &[("destination_kind", &["kind"])];

/// Refuses a space another move already takes.
pub fn insert(tx: &Transaction<'_>, record: &MoveRecord) -> Result<()> {
    if read(tx, &record.space)?.is_some() {
        let message = "another move of this space is recorded; Shoal finishes it first";
        return Err(Refusal::new(Reason::MoveInProgress, message).into());
    }
    let (destination, kind) = destination_columns(&record.destination)?;
    tx.execute(
        "INSERT INTO space_moves(space_id,destination,destination_kind,ticket,carrying,unsaved,
            phase,checkpoints,saved_before,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            record.space,
            destination,
            kind,
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

/// Records the checkpoints its package holds too.
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

pub fn read(db: &Connection, space: &SpaceId) -> Result<Option<MoveRecord>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM space_moves WHERE space_id=?1"),
            [space],
            record,
        )
        .optional()?)
}

pub fn all(db: &Transaction<'_>) -> Result<Vec<MoveRecord>> {
    Ok(db
        .prepare(&format!("SELECT {COLUMNS} FROM space_moves"))?
        .query_map([], record)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Those whose move holds its package for the joined machine that started
/// it and waits for that machine after `now`.
pub(super) fn held_after(db: &Connection, now: UnixTime) -> Result<HashSet<SpaceId>> {
    let (_, held) = destination_columns(&MoveDestination::Held)?;
    Ok(db
        .prepare("SELECT space_id FROM space_moves WHERE destination_kind=?1 AND held_until>?2")?
        .query_map(params![held, now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Does nothing when that machine ended the space already. The move then
/// waits for the machine until `until`.
pub(super) fn read_whole(tx: &Transaction<'_>, space: &SpaceId, until: UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE space_moves SET phase=iif(phase='packing','delivering',phase),held_until=?2
            WHERE space_id=?1",
        params![space, until],
    )?;
    Ok(())
}

pub(super) fn hold_until(tx: &Transaction<'_>, space: &SpaceId, until: UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE space_moves SET held_until=?2 WHERE space_id=?1",
        params![space, until],
    )?;
    Ok(())
}

pub(super) fn forget(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_moves WHERE space_id=?1", [space])?;
    Ok(())
}

fn record(row: &Row<'_>) -> rusqlite::Result<MoveRecord> {
    Ok(MoveRecord {
        space: row.get("space_id")?,
        destination: split::decode_row(row, "destination", DESTINATION)?,
        ticket: row.get("ticket")?,
        carrying: required(row, "carrying")?,
        unsaved: required(row, "unsaved")?,
        phase: row.get::<_, MovePhase>("phase")?,
        checkpoints: required(row, "checkpoints")?,
        saved_before: required(row, "saved_before")?,
    })
}

/// The destination's JSON without its kind, and its kind.
fn destination_columns(destination: &MoveDestination) -> Result<(String, SqlValue)> {
    let (json, mut kind) = split::split(destination, DESTINATION)?;
    Ok((json, kind.remove(0)))
}

/// The JSON column `name`, which is never `NULL`.
fn required<T: serde::de::DeserializeOwned>(row: &Row<'_>, name: &str) -> rusqlite::Result<T> {
    store::json_column(row, name)?
        .ok_or_else(|| rusqlite::Error::InvalidColumnName(name.to_owned()))
}

#[cfg(test)]
mod tests;
