//! Renaming the recorded space names a number could be read as, or that
//! break the space name rule otherwise, so every name reads as a
//! [`SpaceName`]. Nothing else follows a rename: no journal event, message
//! or file move, so a renamed chat's files stay under its old name.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{client::space_names::SpaceName, model::SpaceKind};

pub(super) fn rename_invalid_names(db: &Connection) -> Result<()> {
    let renamed_trigger: Option<String> = db
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='chat_renamed'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    db.execute_batch("DROP TRIGGER IF EXISTS chat_renamed")?;
    // The names spaces hold, which the renamed ones must not take.
    for (table, key, kind) in [
        ("workspaces", "id", SpaceKind::Workspace),
        ("workspace_renames", "workspace_id", SpaceKind::Workspace),
        ("chats", "id", SpaceKind::Chat),
    ] {
        for (id, name) in invalid_names(db, table, key)? {
            let name = super::free_name(db, &SpaceName::derive(kind, &name))?;
            db.execute(
                &format!("UPDATE {table} SET name=?2 WHERE {key}=?1"),
                params![id, name],
            )?;
        }
    }
    for (id, name) in invalid_names(db, "removed_workspaces", "workspace_id")? {
        db.execute(
            "UPDATE removed_workspaces SET name=?2, record=json_set(record,'$.name',?2)
                WHERE workspace_id=?1",
            params![id, SpaceName::derive(SpaceKind::Workspace, &name)],
        )?;
    }
    for (id, name) in invalid_names(db, "agent_queue", "id")? {
        db.execute(
            "UPDATE agent_queue SET name=?2 WHERE id=?1",
            params![id, SpaceName::derive(SpaceKind::Workspace, &name)],
        )?;
    }
    if let Some(trigger) = renamed_trigger {
        db.execute_batch(&trigger)?;
    }
    Ok(())
}

/// The rows of `table`, by `key`, whose name is no valid space name.
fn invalid_names(db: &Connection, table: &str, key: &str) -> Result<Vec<(String, String)>> {
    let names = db
        .prepare(&format!("SELECT {key},name FROM {table}"))?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    Ok(names
        .into_iter()
        .filter(|(_, name)| SpaceName::parse(name).is_err())
        .collect())
}
