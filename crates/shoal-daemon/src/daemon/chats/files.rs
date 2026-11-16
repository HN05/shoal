//! The file a chat started detached keeps under the state directory, its
//! session's log, named after the chat's ID so its name can change freely.
use crate::paths::StateFiles;
use anyhow::{Context as _, Result};
use std::collections::HashSet;

use crate::{
    daemon::workspace::Manager,
    fsutil::off_runtime,
    model::Chat,
    paths::{Paths, SESSION_LOG},
};

impl Manager {
    /// Delete the files in the chats' directory that no chat owns, which
    /// ended chats left.
    pub(crate) async fn remove_unowned_chat_files(&self) -> Result<()> {
        let chats = self.chats().await?;
        let paths = self.paths.clone();
        off_runtime::run(move || remove_unowned(&paths, &chats)).await
    }
}

/// [`Manager::remove_unowned_chat_files`] over the files in the chats'
/// directory.
fn remove_unowned(paths: &Paths, chats: &[Chat]) -> Result<()> {
    let directory = paths.state.join("chats");
    let entries = match std::fs::read_dir(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("cannot read {}", directory.display()))?,
    };
    let owned: HashSet<_> = (chats.iter())
        .map(|chat| paths.chat_files(chat).with_extension(SESSION_LOG))
        .collect();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && !owned.contains(&path) {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot delete {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
