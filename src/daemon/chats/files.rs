//! The files a chat started detached keeps under the state directory, its
//! session's log and first message, named after the chat's ID so its name
//! can change freely.
use anyhow::{Context as _, Result};
use std::{collections::HashMap, path::Path};

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
    let owners: HashMap<&str, &Chat> = (chats.iter())
        .flat_map(|chat| [(chat.id.as_str(), chat), (chat.name.as_str(), chat)])
        .collect();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        match owner(&path, &owners) {
            Some((chat, extension)) => {
                let named = paths.chat_files(chat).with_extension(extension);
                if named != path {
                    std::fs::rename(&path, &named)
                        .with_context(|| format!("cannot move {}", path.display()))?;
                }
            }
            None => std::fs::remove_file(&path)
                .with_context(|| format!("cannot delete {}", path.display()))?,
        }
    }
    Ok(())
}

/// The chat whose ID or name `path`, one of a chat's files, is named after,
/// with the file's extension; none for a file no chat owns.
fn owner<'a>(path: &Path, owners: &HashMap<&str, &'a Chat>) -> Option<(&'a Chat, &'static str)> {
    let file = path.file_name()?.to_str()?;
    [SESSION_LOG, SESSION_PROMPT]
        .into_iter()
        .find_map(|extension| {
            let stem = file.strip_suffix(extension)?.strip_suffix('.')?;
            Some((*owners.get(stem)?, extension))
        })
}

#[cfg(test)]
mod tests;
