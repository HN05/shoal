//! The fill of migration 332: a completion's cleanup error moves from its
//! JSON into a column.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::json_rows::JsonRow;

pub(super) fn move_error(db: &Connection) -> Result<()> {
    let rows = db
        .prepare("SELECT space_id,record FROM space_completion")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text) in rows {
        let mut record = JsonRow::parse("space_completion", id.as_str(), &text)?;
        let error: Option<String> = record.take_optional(&["error"])?;
        db.execute(
            "UPDATE space_completion SET record=?2,error=?3 WHERE space_id=?1",
            params![id, record.text(), error],
        )?;
    }
    Ok(())
}
