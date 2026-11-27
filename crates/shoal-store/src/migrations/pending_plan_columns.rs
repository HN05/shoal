//! The fill of migration 330: the kind of a pending worktree's source and
//! the checkpoint it names move from its plan into columns.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::json_rows::JsonRow;

pub(super) fn move_source_fields(db: &Connection) -> Result<()> {
    let plans = db
        .prepare("SELECT id,pending_plan FROM workspaces WHERE pending_plan IS NOT NULL")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text) in plans {
        let mut plan = JsonRow::parse("workspaces", id.as_str(), &text)?;
        let source: String = plan.take(&["source", "kind"])?;
        let checkpoint: Option<i64> = plan.take_optional(&["source", "checkpoint"])?;
        db.execute(
            "UPDATE workspaces SET pending_plan=?2,pending_source=?3,pending_checkpoint=?4
                WHERE id=?1",
            params![id, plan.text(), source, checkpoint],
        )?;
    }
    Ok(())
}
