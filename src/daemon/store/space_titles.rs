//! A workspace's title merges into its name: each workspace takes its
//! title, else its linked issue's title, else its name, as its list row
//! showed it, cut to the longest name allowed; a removed workspace's record
//! takes its issue's title the same way, and a chat's name is cut to fit.
//! The migration renames without journal events, and a workspace's
//! `renamed` event then records a new name or a branch rename. A cut chat
//! keeps its name before the cut in `chat_legacy_names`, which its files
//! were named after until the daemon moves them.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::client::space_names::SpaceName;

pub(super) fn names_from_titles(db: &Connection) -> Result<()> {
    let chat_renamed = drop_rename_triggers(db)?;
    let titled = has_titles(db)?;
    rename_workspaces(db, titled)?;
    rename_removed_workspaces(db)?;
    create_legacy_chat_names(db)?;
    cut_chat_names(db)?;
    name_title_events(db)?;
    if titled {
        drop_titles(db)?;
    }
    create_rename_triggers(db, chat_renamed)
}

/// Drop the triggers that journal renames, so the migration's renames add
/// no events, returning the chat's to create again.
fn drop_rename_triggers(db: &Connection) -> Result<Option<String>> {
    let chat_renamed = db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='chat_renamed'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    db.execute_batch(
        "DROP TRIGGER IF EXISTS chat_renamed;
         DROP TRIGGER IF EXISTS workspace_name_changed;
         DROP TRIGGER IF EXISTS workspace_renamed;",
    )?;
    Ok(chat_renamed)
}

/// Whether workspaces have titles, which a database that tests emulate
/// from before titles lacks.
fn has_titles(db: &Connection) -> Result<bool> {
    Ok(db
        .prepare("SELECT 1 FROM pragma_table_info('workspaces') WHERE name='title'")?
        .exists([])?)
}

fn drop_titles(db: &Connection) -> Result<()> {
    db.execute_batch("ALTER TABLE workspaces DROP COLUMN title")?;
    Ok(())
}

/// Create the chat's rename trigger `chat_renamed` again, and the
/// workspace's, which journals a new name or branch.
fn create_rename_triggers(db: &Connection, chat_renamed: Option<String>) -> Result<()> {
    if let Some(trigger) = chat_renamed {
        db.execute_batch(&trigger)?;
    }
    db.execute_batch(RENAMED)?;
    Ok(())
}

/// A workspace's new name or branch, which `shoal rename` and a branch
/// rename give it.
const RENAMED: &str = "
CREATE TRIGGER workspace_name_changed AFTER UPDATE OF name, branch ON workspaces
WHEN NEW.name IS NOT OLD.name OR NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;";

/// The name a workspace takes from the first of `labels` that is not
/// empty.
fn name_from(labels: [Option<String>; 3]) -> Option<SpaceName> {
    let label = labels
        .into_iter()
        .flatten()
        .find(|label| !label.trim().is_empty())?;
    Some(SpaceName::derive(&label, "workspace"))
}

fn rename_workspaces(db: &Connection, titled: bool) -> Result<()> {
    let title = if titled { "title" } else { "NULL" };
    let workspaces = db
        .prepare(&format!(
            "SELECT id,{title},(SELECT title FROM space_issue WHERE space_id=workspaces.id),name
                FROM workspaces"
        ))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                [row.get(1)?, row.get(2)?, row.get(3)?],
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, labels) in workspaces {
        if let Some(name) = name_from(labels) {
            db.execute(
                "UPDATE workspaces SET name=?2 WHERE id=?1",
                params![id, name],
            )?;
        }
    }
    Ok(())
}

fn rename_removed_workspaces(db: &Connection) -> Result<()> {
    let removed = db
        .prepare(
            "SELECT workspace_id,json_extract(record,'$.title'),
                    json_extract(record,'$.links.issue_title'),name
                FROM removed_workspaces WHERE json_valid(record)",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                [row.get(1)?, row.get(2)?, row.get(3)?],
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, labels) in removed {
        let name = name_from(labels);
        db.execute(
            "UPDATE removed_workspaces SET name=coalesce(?2,name),
                    record=json_remove(json_set(record,'$.name',coalesce(?2,name)),'$.title')
                WHERE workspace_id=?1",
            params![id, name],
        )?;
    }
    Ok(())
}

/// Rewrite each `renamed` event that recorded a new title as one recording
/// the name it now is, so a space's log reads it as it happened; one that
/// cleared the title keeps the name it recorded.
fn name_title_events(db: &Connection) -> Result<()> {
    db.execute_batch(
        "UPDATE workspace_events
            SET record=json_set(record,'$.name',json_extract(record,'$.title'))
            WHERE json_valid(record) AND json_extract(record,'$.kind')='renamed'
                AND json_type(record,'$.title')='text';
         UPDATE workspace_events SET record=json_remove(record,'$.title')
            WHERE json_valid(record) AND json_extract(record,'$.kind')='renamed'
                AND json_type(record,'$.title') IS NOT NULL;",
    )?;
    Ok(())
}

/// The names chats had before the cut, which their files were named
/// after; the daemon empties it once it has moved them.
fn create_legacy_chat_names(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_legacy_names (
            chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
            name TEXT NOT NULL
        );",
    )?;
    Ok(())
}

/// Cut each chat's name to fit, recording the name it had.
fn cut_chat_names(db: &Connection) -> Result<()> {
    let chats = db
        .prepare("SELECT id,name FROM chats")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, name) in chats {
        let cut = SpaceName::derive(&name, "chat");
        if cut != name {
            db.execute(
                "INSERT OR REPLACE INTO chat_legacy_names(chat_id,name) VALUES (?1,?2)",
                params![id, name],
            )?;
            db.execute("UPDATE chats SET name=?2 WHERE id=?1", params![id, cut])?;
        }
    }
    Ok(())
}
