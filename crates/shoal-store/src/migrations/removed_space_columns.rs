//! The fill of migration 331: a removal record's fields that its columns
//! keep leave its JSON, and the workspace a chat ran in and the expiry of a
//! workspace's checkpoint move into columns of their own.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::Value;

use super::json_rows::JsonRow;

pub(super) fn move_fields(db: &Connection) -> Result<()> {
    let rows = db
        .prepare("SELECT space_id,repository_id,chat,name,expires_at,record FROM removed_spaces")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, repository, chat, name, expires_at, text) in rows {
        let mut record = JsonRow::parse("removed_spaces", id.as_str(), &text)?;
        record.take_matching(&["workspace_id"], &Value::from(id.as_str()))?;
        record.take_matching(&["repository_id"], &Value::from(repository))?;
        record.take_matching(&["name"], &Value::from(name))?;
        record.take_matching(&["expires_at"], &Value::from(expires_at))?;
        let (workspace, checkpoint_expires_at): (Option<String>, Option<i64>) = match chat {
            true => (record.take_optional(&["chat", "workspace", "id"])?, None),
            false => (None, Some(record.take(&["checkpoint", "expires_at"])?)),
        };
        db.execute(
            "UPDATE removed_spaces SET record=?2,chat_workspace_id=?3,checkpoint_expires_at=?4
                WHERE space_id=?1",
            params![id, record.text(), workspace, checkpoint_expires_at],
        )?;
    }
    Ok(())
}
