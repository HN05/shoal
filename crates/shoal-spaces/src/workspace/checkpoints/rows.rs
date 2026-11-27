//! The rows of the checkpoints table: a checkpoint's columns, and the JSON
//! of the fields no query reads.
use anyhow::Result;
use rusqlite::{Connection, Row, params};
use shoal_store::split::{self, Column};

use crate::model::Checkpoint;

/// Each column, with the field of the checkpoint it keeps.
const FIELDS: &[Column] = &[
    ("workspace_id", &["workspace_id"]),
    ("number", &["number"]),
    ("repository_id", &["repository_id"]),
    ("expires_at", &["expires_at"]),
];

/// What a query selects to read checkpoints of the table `alias`.
pub(crate) fn columns(alias: &str) -> String {
    let mut columns: Vec<String> = (FIELDS.iter())
        .map(|(column, _)| format!("{alias}.{column}"))
        .collect();
    columns.push(format!("{alias}.record"));
    columns.join(",")
}

pub(crate) fn insert(db: &Connection, checkpoint: &Checkpoint) -> Result<()> {
    db.execute(
        "INSERT INTO checkpoints(workspace_id,number,repository_id,expires_at,record)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            checkpoint.workspace_id,
            checkpoint.number,
            checkpoint.repository_id,
            checkpoint.expires_at,
            split::json_without(checkpoint, FIELDS)?,
        ],
    )?;
    Ok(())
}

/// Reads the columns [`columns`] selects.
pub(crate) fn checkpoint(row: &Row<'_>) -> rusqlite::Result<Checkpoint> {
    split::decode_row(row, "record", FIELDS)
}

#[cfg(test)]
mod tests;
