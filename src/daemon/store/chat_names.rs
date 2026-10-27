//! Names for the chats a store recorded by number, before
//! `chat_names.sql` drops the numbers.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{
    daemon::{chats::suffixed, workspace::derive_workspace_name},
    forge::repo::remote_url::source_name,
};

/// Name each recorded chat, and the chats of journal events, after its
/// repository and number as lists showed them.
pub(super) fn name_chats(db: &Connection) -> Result<()> {
    db.execute_batch("ALTER TABLE chats ADD COLUMN name TEXT;")?;
    let bases = ChatBases::load(db)?;
    let named = name_recorded_chats(db, &bases)?;
    name_journal_chats(db, &bases, &named)
}

/// Each repository's `<repo>-chat`, as a valid name.
struct ChatBases(HashMap<String, String>);

impl ChatBases {
    fn load(db: &Connection) -> Result<Self> {
        let bases = db
            .prepare("SELECT id,name,source FROM repositories")?
            .query_map([], |row| {
                let (id, name, source): (String, Option<String>, String) =
                    (row.get(0)?, row.get(1)?, row.get(2)?);
                let name = name.unwrap_or_else(|| source_name(&source).to_owned());
                Ok((id, derive_workspace_name(&format!("{name}-chat"))))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self(bases))
    }

    /// The name of chat `number` of `repository`; a removed repository's
    /// chats are plain `chat`.
    fn name(&self, repository: &str, number: u32) -> String {
        let base = self.0.get(repository).map_or("chat", String::as_str);
        suffixed(base, u64::from(number))
    }
}

/// Name each recorded chat by its number, or the next one past names that
/// workspaces, unfinished renames and earlier chats hold. Returns the names
/// by chat ID.
fn name_recorded_chats(db: &Connection, bases: &ChatBases) -> Result<HashMap<String, String>> {
    let mut taken: HashSet<String> = db
        .prepare("SELECT name FROM workspaces UNION SELECT name FROM workspace_renames")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let chats = db
        .prepare("SELECT id,repository_id,number FROM chats ORDER BY repository_id,number")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, u32)>>>()?;
    let mut named = HashMap::new();
    for (id, repository, number) in chats {
        let name = (number..)
            .map(|number| bases.name(&repository, number))
            .find(|name| !taken.contains(name))
            .expect("an unused suffix exists");
        db.execute("UPDATE chats SET name=?2 WHERE id=?1", params![id, name])?;
        taken.insert(name.clone());
        named.insert(id, name);
    }
    Ok(named)
}

/// Replace the number of each journal event's chat with the name the chat
/// was given, or for an ended chat the name its number gives.
fn name_journal_chats(
    db: &Connection,
    bases: &ChatBases,
    named: &HashMap<String, String>,
) -> Result<()> {
    let events = db
        .prepare(
            "SELECT id,json_extract(record,'$.chat.id'),json_extract(record,'$.chat.repository_id'),
                json_extract(record,'$.chat.number')
             FROM workspace_events
             WHERE json_extract(record,'$.kind') IN ('chat_started','chat_ended')",
        )?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, String, u32)>>>()?;
    for (event, chat, repository, number) in events {
        let name = named
            .get(&chat)
            .cloned()
            .unwrap_or_else(|| bases.name(&repository, number));
        db.execute(
            "UPDATE workspace_events
             SET record=json_remove(json_set(record,'$.chat.name',?2),'$.chat.number') WHERE id=?1",
            params![event, name],
        )?;
    }
    Ok(())
}
