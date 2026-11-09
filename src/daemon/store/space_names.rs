//! Renaming the recorded space names a number could be read as, or that
//! break the space name rule otherwise, and the copies other records keep
//! of them, so every name reads as a [`SpaceName`]. Nothing else follows a
//! rename: no journal event, message or file move, so a renamed chat's
//! files stay under its old name.
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

/// How a copy of a space's name that is empty, or no valid name, reads
/// once rewritten.
#[derive(Clone, Copy)]
enum Rewrite {
    /// The name a space of this kind derives from it.
    Derive(SpaceKind),
    /// No name for an empty copy, which says none was known; otherwise as
    /// [`Rewrite::Derive`].
    ClearEmpty(SpaceKind),
}

impl Rewrite {
    /// The copy `name` becomes; none when it goes.
    fn of(self, name: &str) -> Option<SpaceName> {
        match self {
            Self::ClearEmpty(_) if name.is_empty() => None,
            Self::Derive(kind) | Self::ClearEmpty(kind) => Some(SpaceName::derive(kind, name)),
        }
    }
}

/// Rewrite the copies of space names that records keep beside the spaces,
/// as [`rename_invalid_names`] renamed the spaces, so every copy reads as a
/// [`SpaceName`]: those of notifications, attached files, simulator clean
/// requests, approval requests and journal events.
pub(super) fn rename_invalid_copies(db: &Connection) -> Result<()> {
    rename_column(
        db,
        "notifications",
        "workspace",
        Rewrite::ClearEmpty(SpaceKind::Workspace),
    )?;
    for (chat, kind) in [(false, SpaceKind::Workspace), (true, SpaceKind::Chat)] {
        let files = format!(
            "(SELECT * FROM workspace_files WHERE (space_id IN (SELECT id FROM chats)
                OR space_id IN (SELECT chat_id FROM ended_chats))={})",
            u8::from(chat)
        );
        for (id, name) in invalid_copies(db, &files, "id", "space")? {
            let name = SpaceName::derive(kind, &name);
            db.execute(
                "UPDATE workspace_files SET space=?2 WHERE id=?1",
                params![id, name],
            )?;
        }
    }
    let workspace = Rewrite::Derive(SpaceKind::Workspace);
    rename_in_records(
        db,
        "simulator_clean_requests",
        "$.workspace_name",
        workspace,
    )?;
    let space = Rewrite::ClearEmpty(SpaceKind::Workspace);
    rename_in_records(db, "access_requests", "$.space", space)?;
    for (path, kind) in [
        ("$.name", SpaceKind::Workspace),
        ("$.chat.name", SpaceKind::Chat),
        ("$.file.workspace", SpaceKind::Workspace),
        ("$.file.chat", SpaceKind::Chat),
    ] {
        rename_in_records(db, "workspace_events", path, Rewrite::Derive(kind))?;
    }
    Ok(())
}

/// Rewrite each copy in `column` of `table` that is no valid space name.
fn rename_column(db: &Connection, table: &str, column: &str, rewrite: Rewrite) -> Result<()> {
    for (id, name) in invalid_copies(db, table, "rowid", column)? {
        db.execute(
            &format!("UPDATE {table} SET {column}=?2 WHERE rowid=?1"),
            params![id, rewrite.of(&name)],
        )?;
    }
    Ok(())
}

/// Rewrite each copy at `path` in the JSON `record` of `table` that is no
/// valid space name; a copy that goes leaves the record without it.
fn rename_in_records(db: &Connection, table: &str, path: &str, rewrite: Rewrite) -> Result<()> {
    let records = format!(
        "(SELECT rowid AS key, json_extract(record,'{path}') AS copy FROM {table}
            WHERE json_valid(record) AND json_type(record,'{path}')='text')"
    );
    for (key, name) in invalid_copies(db, &records, "key", "copy")? {
        match rewrite.of(&name) {
            Some(name) => db.execute(
                &format!("UPDATE {table} SET record=json_set(record,'{path}',?2) WHERE rowid=?1"),
                params![key, name],
            )?,
            None => db.execute(
                &format!("UPDATE {table} SET record=json_remove(record,'{path}') WHERE rowid=?1"),
                [key],
            )?,
        };
    }
    Ok(())
}

/// The rows of `rows`, a table or subquery, by `key`, whose `column` holds
/// a name that is no valid space name.
fn invalid_copies(
    db: &Connection,
    rows: &str,
    key: &str,
    column: &str,
) -> Result<Vec<(i64, String)>> {
    let copies = db
        .prepare(&format!(
            "SELECT {key},{column} FROM {rows} WHERE {column} IS NOT NULL"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(copies
        .into_iter()
        .filter(|(_, name)| SpaceName::parse(name).is_err())
        .collect())
}
