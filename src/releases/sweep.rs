//! Removing the release copies no wrapper runs from.
use std::{
    fs::{self, File},
    io,
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result, ensure};

use super::{LOCK, Releases, TEMPORARY};

/// How old a temporary directory gets before the sweep takes it for one an
/// interrupted copy or removal left behind.
const ABANDONED: Duration = Duration::from_secs(60 * 60);

impl Releases {
    /// Remove every copy whose lock no process holds, and the temporary
    /// directories older than [`ABANDONED`] at `now`, returning the names of
    /// the removed copies. A failure with one entry does not skip the others.
    pub(crate) fn sweep(&self, now: SystemTime) -> Result<Vec<String>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("list {}", self.root.display()));
            }
        };
        let mut removed = Vec::new();
        let mut failed = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let result = if name.starts_with(TEMPORARY) {
                remove_abandoned(&path, now)
            } else {
                self.remove_unused(&path).map(|done| {
                    if done {
                        removed.push(name.clone());
                    }
                })
            };
            if let Err(error) = result {
                failed.push(format!("{name}: {error}"));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(removed)
    }

    /// Remove `copy` unless a process holds its lock; whether it went.
    fn remove_unused(&self, copy: &Path) -> io::Result<bool> {
        let lock = match File::open(copy.join(LOCK)) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => return Ok(false),
            Err(error) => return Err(error),
        }
        // Moved aside while locked, so a wrapper waiting for the lock finds
        // its copy gone instead of locking one being deleted.
        let removing = self
            .root
            .join(format!("{TEMPORARY}{}", uuid::Uuid::new_v4()));
        fs::rename(copy, &removing)?;
        drop(lock);
        fs::remove_dir_all(&removing)?;
        Ok(true)
    }
}

/// Remove the temporary directory `path` once it is older than [`ABANDONED`].
fn remove_abandoned(path: &Path, now: SystemTime) -> io::Result<()> {
    let modified = fs::symlink_metadata(path)?.modified()?;
    if now.duration_since(modified).unwrap_or_default() < ABANDONED {
        return Ok(());
    }
    fs::remove_dir_all(path)
}

#[cfg(test)]
pub(super) mod tests;
