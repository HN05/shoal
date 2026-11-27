//! The fill of migration 335: a move destination's kind moves from its
//! JSON into a column.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::json_rows::JsonRow;

pub(super) fn move_kind(db: &Connection) -> Result<()> {
    let rows = db
        .prepare("SELECT space_id,destination FROM space_moves")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text) in rows {
        let mut destination = JsonRow::parse("space_moves", id.as_str(), &text)?;
        let kind: String = destination.take(&["kind"])?;
        db.execute(
            "UPDATE space_moves SET destination=?2,destination_kind=?3 WHERE space_id=?1",
            params![id, destination.text(), kind],
        )?;
    }
    Ok(())
}
