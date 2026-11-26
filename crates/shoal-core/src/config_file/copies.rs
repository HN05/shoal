//! Config files as the hub's copies replace them, under the file's edit
//! lock, which its key edits take too: the shared files, under the shared
//! config's lock, and a shared repository's local config, under its own.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{keep_backup, lock, refuse_user_config, replace_keeping};

/// A config file's edit lock, which a machine holds while it compares the
/// files it guards with the hub's copies and writes them.
pub struct Held {
    _lock: fs::File,
}

/// Hold the edit lock of the config file at `path` while the returned
/// value lives.
pub fn hold(path: &Path) -> Result<Held> {
    let _lock = lock(path)?;
    Ok(Held { _lock })
}

impl Held {
    /// Make the file at `path`, one the lock guards, read `text`, removing
    /// it for none. With `keep`, the file it replaces stays as
    /// `<file>.backup`, which is returned once there was a file to keep.
    pub fn write(&self, path: &Path, text: Option<&str>, keep: bool) -> Result<Option<PathBuf>> {
        let backup = keep.then(|| backup_of(path));
        let kept = match text {
            Some(text) => replace_keeping(path, text, backup.as_deref())?,
            None => remove_keeping(path, backup.as_deref())?,
        };
        Ok(backup.filter(|_| kept))
    }
}

/// Remove `path`, first leaving a copy of it at `backup`, when given.
/// Whether there was a file to keep.
fn remove_keeping(path: &Path, backup: Option<&Path>) -> Result<bool> {
    refuse_user_config(path)?;
    let kept = backup.map(|backup| {
        keep_backup(path, backup)
            .with_context(|| format!("keep {} as {}", path.display(), backup.display()))
    });
    let kept = kept.transpose()?.unwrap_or(false);
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", path.display()))
        }
        _ => Ok(kept),
    }
}

fn backup_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".backup");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests;
