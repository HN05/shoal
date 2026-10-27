//! Names for the chats a store recorded by number, before
//! `chat_names.sql` drops the numbers.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{
    daemon::{chats::suffixed, workspace::derive_workspace_name},
    forge::repo::remote_url::source_name,
};

/// Name each recorded chat `<repo>-chat`, numbered after the first as lists
/// showed it, past names workspaces and earlier chats hold. Journal events
/// name a recorded chat the same, and an ended one by its number alone.
pub(super) fn name_chats(db: &Connection) -> Result<()> {
    db.execute_batch("ALTER TABLE chats ADD COLUMN name TEXT;")?;
    let bases = chat_bases(db)?;
    let base = |repository: &str| bases.get(repository).map_or("chat", String::as_str);
    let mut taken: HashSet<String> = db
        .prepare("SELECT name FROM workspaces UNION SELECT name FROM workspace_renames")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let chats = db
        .prepare("SELECT id,repository_id,number FROM chats ORDER BY repository_id,number")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, u32)>>>()?;
    let mut named = HashMap::new();
    for (id, repository, number) in chats {
        let name = (u64::from(number)..)
            .map(|suffix| suffixed(base(&repository), suffix))
            .find(|name| !taken.contains(name))
            .expect("an unused suffix exists");
        db.execute("UPDATE chats SET name=?2 WHERE id=?1", params![id, name])?;
        taken.insert(name.clone());
        named.insert(id, name);
    }
    let events = db
        .prepare(
            "SELECT id,json_extract(record,'$.chat.id'),json_extract(record,'$.chat.repository_id'),
                json_extract(record,'$.chat.number')
             FROM workspace_events
             WHERE json_extract(record,'$.kind') IN ('chat_started','chat_ended')",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, String, u32)>>>()?;
    for (event, chat, repository, number) in events {
        let name = named
            .get(&chat)
            .cloned()
            .unwrap_or_else(|| suffixed(base(&repository), u64::from(number)));
        db.execute(
            "UPDATE workspace_events
             SET record=json_remove(json_set(record,'$.chat.name',?2),'$.chat.number') WHERE id=?1",
            params![event, name],
        )?;
    }
    Ok(())
}

/// Each repository's `<repo>-chat`, as a valid name.
fn chat_bases(db: &Connection) -> Result<HashMap<String, String>> {
    Ok(db
        .prepare("SELECT id,name,source FROM repositories")?
        .query_map([], |row| {
            let (id, name, source): (String, Option<String>, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let name = name.unwrap_or_else(|| source_name(&source).to_owned());
            Ok((id, derive_workspace_name(&format!("{name}-chat"))))
        })?
        .collect::<rusqlite::Result<_>>()?)
}
