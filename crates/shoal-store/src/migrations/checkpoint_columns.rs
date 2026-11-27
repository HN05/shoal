//! The preparation of migration 329. A checkpoint's record leaves out the
//! fields its row keeps in columns, which it held as well.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::Value;

use super::json_rows::JsonRow;

pub(super) fn drop_column_fields(db: &Connection) -> Result<()> {
    let rows = db
        .prepare("SELECT workspace_id,number,repository_id,expires_at,record FROM checkpoints")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (workspace, number, repository, expires_at, text) in rows {
        let mut record = JsonRow::parse("checkpoints", format!("{workspace}/{number}"), &text)?;
        record.take_matching(&["workspace_id"], &Value::from(workspace.as_str()))?;
        record.take_matching(&["number"], &Value::from(number))?;
        record.take_matching(&["repository_id"], &Value::from(repository))?;
        record.take_matching(&["expires_at"], &Value::from(expires_at))?;
        db.execute(
            "UPDATE checkpoints SET record=?3 WHERE workspace_id=?1 AND number=?2",
            params![workspace, number, record.text()],
        )?;
    }
    Ok(())
}
