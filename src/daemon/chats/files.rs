//! The files a chat started detached keeps under the state directory, its
//! session's log and first message, named after the chat's ID so its name
//! can change freely.
use anyhow::{Context as _, Result};
use std::{collections::HashMap, path::Path};

use crate::client::space_ids::SpaceId;
use crate::{
    daemon::workspace::Manager,
    fsutil::off_runtime,
    model::Chat,
    paths::{Paths, SESSION_LOG, SESSION_PROMPT},
};

impl Manager {
    /// Name the files each chat kept under its name after its ID instead,
    /// and delete the files no chat owns, which ended chats left. A chat
    /// whose name the upgrade cut owns those named as it was before.
    pub(crate) async fn rekey_chat_files(&self) -> Result<()> {
        let chats = self.chats().await?;
        let legacy = self.store.run(|db| legacy_names(db)).await?;
        let paths = self.paths.clone();
        off_runtime::run(move || rekey(&paths, &chats, &legacy)).await?;
        self.store.write(forget_legacy_names).await
    }
}

/// Each cut chat with the name it had before the upgrade cut it.
fn legacy_names(db: &rusqlite::Connection) -> Result<Vec<(SpaceId, String)>> {
    Ok(db
        .prepare("SELECT chat_id,name FROM chat_legacy_names")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Forget the names before the cut once the files named after them moved.
fn forget_legacy_names(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM chat_legacy_names", [])?;
    Ok(())
}

/// [`Manager::rekey_chat_files`] over the files in the chats' directory.
fn rekey(paths: &Paths, chats: &[Chat], legacy: &[(SpaceId, String)]) -> Result<()> {
    let directory = paths.state.join("chats");
    let entries = match std::fs::read_dir(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("cannot read {}", directory.display()))?,
    };
    let owners = owners(chats, legacy);
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        match owner(&path, &owners) {
            Owner::Chat(chat, extension) => {
                let named = paths.chat_files(chat).with_extension(extension);
                // A file the chat has under its ID already stays its own.
                if named != path && !named.exists() {
                    std::fs::rename(&path, &named)
                        .with_context(|| format!("cannot move {}", path.display()))?;
                }
            }
            Owner::Unclear => {}
            Owner::None => std::fs::remove_file(&path)
                .with_context(|| format!("cannot delete {}", path.display()))?,
        }
    }
    Ok(())
}

/// The chat each file stem names: its ID, its name, or its name before the
/// upgrade cut it. Names repeat now, so a stem several chats have names
/// none for sure.
fn owners<'a>(
    chats: &'a [Chat],
    legacy: &[(SpaceId, String)],
) -> HashMap<String, Option<&'a Chat>> {
    let mut owners: HashMap<String, Option<&Chat>> = HashMap::new();
    let mut add = |stem: &str, chat: &'a Chat| {
        (owners.entry(stem.to_owned()))
            .and_modify(|owner| {
                if owner.is_some_and(|owner| owner.id != chat.id) {
                    *owner = None;
                }
            })
            .or_insert(Some(chat));
    };
    for chat in chats {
        add(chat.name.as_str(), chat);
    }
    for (id, name) in legacy {
        if let Some(chat) = chats.iter().find(|chat| chat.id == *id) {
            add(name, chat);
        }
    }
    for chat in chats {
        owners.insert(chat.id.to_string(), Some(chat));
    }
    owners
}

/// Who owns one of a chat's files.
enum Owner<'a> {
    /// The chat, and the file's extension.
    Chat(&'a Chat, &'static str),
    /// Chats that share the name the file was named after, which keep it.
    Unclear,
    /// No chat: one that ended left it.
    None,
}

/// The chat whose ID or name `path`, one of a chat's files, is named after.
fn owner<'a>(path: &Path, owners: &HashMap<String, Option<&'a Chat>>) -> Owner<'a> {
    let Some(file) = path.file_name().and_then(|file| file.to_str()) else {
        return Owner::None;
    };
    let found = [SESSION_LOG, SESSION_PROMPT]
        .into_iter()
        .find_map(|extension| {
            let stem = file.strip_suffix(extension)?.strip_suffix('.')?;
            Some((*owners.get(stem)?, extension))
        });
    match found {
        Some((Some(chat), extension)) => Owner::Chat(chat, extension),
        Some((None, _)) => Owner::Unclear,
        None => Owner::None,
    }
}

#[cfg(test)]
mod tests;
