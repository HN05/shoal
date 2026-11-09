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

/// Whether an empty copy of a space's name goes, which says no name was
/// known, rather than becoming the name it derives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Derive,
    Clear,
}

/// What follows a space's ID in SQL to test that the ID is a chat's,
/// running or ended.
const CHAT_ID: &str = "IN (SELECT id FROM chats UNION ALL SELECT chat_id FROM ended_chats)";

/// Rewrite the copies of space names that records keep beside the spaces,
/// as [`rename_invalid_names`] renamed the spaces, so every copy reads as a
/// [`SpaceName`], taking a chat's kind where the copy names a chat: those
/// of notifications, attached files, simulator clean requests, approval
/// requests and journal events.
pub(super) fn rename_invalid_copies(db: &Connection) -> Result<()> {
    let notifications_chat = format!("workspace_id {CHAT_ID}");
    rename_column(
        db,
        "notifications",
        "workspace",
        &notifications_chat,
        Empty::Clear,
    )?;
    let files_chat = format!("space_id {CHAT_ID}");
    rename_column(db, "workspace_files", "space", &files_chat, Empty::Derive)?;
    let simulator = "$.workspace_name";
    rename_in_records(
        db,
        "simulator_clean_requests",
        simulator,
        "0",
        Empty::Derive,
    )?;
    let requests_chat = format!("json_extract(record,'$.space_id') {CHAT_ID}");
    rename_in_records(
        db,
        "access_requests",
        "$.space",
        &requests_chat,
        Empty::Clear,
    )?;
    for (path, chat) in [
        ("$.name", "json_extract(record,'$.chat_id') IS NOT NULL"),
        ("$.chat.name", "1"),
        ("$.file.workspace", "0"),
        ("$.file.chat", "1"),
    ] {
        rename_in_records(db, "workspace_events", path, chat, Empty::Derive)?;
    }
    Ok(())
}

/// Rewrite each copy in `column` of `table` that is no valid space name,
/// where `chat` says whether the row's copy names a chat.
fn rename_column(
    db: &Connection,
    table: &str,
    column: &str,
    chat: &str,
    empty: Empty,
) -> Result<()> {
    let rows = format!("SELECT rowid,{column},COALESCE({chat},0) FROM {table}");
    for (id, name) in invalid_copies(db, &rows, empty)? {
        db.execute(
            &format!("UPDATE {table} SET {column}=?2 WHERE rowid=?1"),
            params![id, name],
        )?;
    }
    Ok(())
}

/// Rewrite each copy at `path` in the JSON `record` of `table` that is no
/// valid space name, as [`rename_column`] does; a copy that goes leaves the
/// record without it.
fn rename_in_records(
    db: &Connection,
    table: &str,
    path: &str,
    chat: &str,
    empty: Empty,
) -> Result<()> {
    let rows = format!(
        "SELECT rowid,json_extract(record,'{path}'),COALESCE({chat},0) FROM {table}
            WHERE json_valid(record) AND json_type(record,'{path}')='text'"
    );
    for (key, name) in invalid_copies(db, &rows, empty)? {
        match name {
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

/// The copies `rows` selects, each a key, a name and whether it names a
/// chat, that are no valid space name, by key with what each becomes: the
/// name it derives for its kind, or none for an empty copy that `empty`
/// clears.
fn invalid_copies(
    db: &Connection,
    rows: &str,
    empty: Empty,
) -> Result<Vec<(i64, Option<SpaceName>)>> {
    let copies = db
        .prepare(rows)?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get::<_, Option<String>>(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, Option<String>, bool)>>>()?;
    Ok(copies
        .into_iter()
        .filter_map(|(key, name, chat)| Some((key, name?, chat)))
        .filter(|(_, name, _)| SpaceName::parse(name).is_err())
        .map(|(key, name, chat)| {
            let kind = if chat {
                SpaceKind::Chat
            } else {
                SpaceKind::Workspace
            };
            let rewritten = (empty == Empty::Derive || !name.is_empty())
                .then(|| SpaceName::derive(kind, &name));
            (key, rewritten)
        })
        .collect())
}
