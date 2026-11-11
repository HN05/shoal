//! The files a chat started detached keeps under the state directory, its
//! session's log and first message, named after the chat's ID so its name
//! can change freely.
use anyhow::{Context as _, Result};
use std::{collections::HashMap, path::Path};

use crate::client::space_names::SpaceName;
use crate::{
    daemon::workspace::Manager,
    fsutil::off_runtime,
    model::Chat,
    paths::{Paths, SESSION_LOG, SESSION_PROMPT},
};

impl Manager {
    /// Name the files each chat kept under its name after its ID instead,
    /// and delete the files no chat owns, which ended chats left.
    pub(crate) async fn rekey_chat_files(&self) -> Result<()> {
        let chats = self.chats().await?;
        let paths = self.paths.clone();
        off_runtime::run(move || rekey(&paths, &chats)).await
    }
}

/// [`Manager::rekey_chat_files`] over the files in the chats' directory.
fn rekey(paths: &Paths, chats: &[Chat]) -> Result<()> {
    let directory = paths.state.join("chats");
    let entries = match std::fs::read_dir(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("cannot read {}", directory.display()))?,
    };
    // Names were unique while files took them; a name the upgrade cut to
    // fit may now be another chat's too, which leaves its files unowned.
    let mut owners: HashMap<&str, Option<&Chat>> = HashMap::new();
    for chat in chats {
        owners.insert(chat.id.as_str(), Some(chat));
        (owners.entry(chat.name.as_str()))
            .and_modify(|owner| *owner = None)
            .or_insert(Some(chat));
    }
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        match owner(&path, &owners) {
            Owner::Chat(chat, extension) => {
                let named = paths.chat_files(chat).with_extension(extension);
                if named != path {
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

/// Who owns one of a chat's files.
enum Owner<'a> {
    /// The chat, and the file's extension.
    Chat(&'a Chat, &'static str),
    /// Chats that share the name the file was named after, which keep it.
    Unclear,
    /// No chat: one that ended left it.
    None,
}

/// The chat whose ID or name `path`, one of a chat's files, is named after,
/// as the upgrade cut a long name to fit.
fn owner<'a>(path: &Path, owners: &HashMap<&str, Option<&'a Chat>>) -> Owner<'a> {
    let Some(file) = path.file_name().and_then(|file| file.to_str()) else {
        return Owner::None;
    };
    let found = [SESSION_LOG, SESSION_PROMPT]
        .into_iter()
        .find_map(|extension| {
            let stem = file.strip_suffix(extension)?.strip_suffix('.')?;
            let cut = SpaceName::derive(stem, "chat");
            let owner = (owners.get(stem)).or_else(|| owners.get(cut.as_str()))?;
            Some((*owner, extension))
        });
    match found {
        Some((Some(chat), extension)) => Owner::Chat(chat, extension),
        Some((None, _)) => Owner::Unclear,
        None => Owner::None,
    }
}

#[cfg(test)]
mod tests;
