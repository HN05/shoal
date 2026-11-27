//! The rows of `space_completion`: a completion's cleanup error, which a
//! trigger tests, in a column, and the rest of it as JSON.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter};
use shoal_ids::space_ids::SpaceId;
use shoal_store::split::{self, Column};

use crate::model::{Completion, EventCause};

/// Each column, with the field of the completion it keeps.
const FIELDS: &[Column] = &[("error", &["error"])];

/// What a query selects to read a completion.
pub(crate) const COLUMNS: &str = "record,error";

/// Reads the columns [`COLUMNS`] names; none where a join found no
/// completion.
pub(crate) fn completion(row: &Row<'_>) -> rusqlite::Result<Option<Completion>> {
    split::decode_optional_row(row, "record", FIELDS)
}

pub(crate) fn read(db: &Connection, id: &SpaceId) -> Result<Option<Completion>> {
    Ok(read_with_cause(db, id)?.map(|(completion, _)| completion))
}

/// The completion with what caused it.
pub(crate) fn read_with_cause(
    db: &Connection,
    id: &SpaceId,
) -> Result<Option<(Completion, EventCause)>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS},cause FROM space_completion WHERE space_id=?1"),
            [id],
            |row| Ok(completion(row)?.zip(Some(row.get("cause")?))),
        )
        .optional()?
        .flatten())
}

/// Replaces the space's completion.
pub(crate) fn write(
    db: &Connection,
    id: &SpaceId,
    completion: &Completion,
    cause: EventCause,
) -> Result<()> {
    let (record, columns) = split::split(completion, FIELDS)?;
    let values = [
        id.to_string().into(),
        record.into(),
        cause.to_string().into(),
    ];
    db.execute(
        "INSERT INTO space_completion(space_id,record,cause,error) VALUES (?1,?2,?3,?4)
         ON CONFLICT(space_id) DO UPDATE
         SET record=excluded.record,cause=excluded.cause,error=excluded.error",
        params_from_iter(values.into_iter().chain(columns)),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
