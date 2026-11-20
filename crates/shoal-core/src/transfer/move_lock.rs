//! The lock a move's worker holds on a space while it moves it, which tells
//! the daemon whether a recorded move still has a worker or needs finishing.
use std::{fs::File, path::PathBuf};

use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::paths::Paths;

/// A held lock on the move of one space, released when it drops.
pub struct MoveLock {
    path: PathBuf,
    _file: File,
}

impl MoveLock {
    /// Wait for the lock on the move of the space `id`, which another
    /// worker holds while it moves that space.
    pub async fn take(paths: &Paths, id: &SpaceId) -> Result<Self> {
        let path = lock_path(paths, id)?;
        tokio::task::spawn_blocking(move || {
            let file = open(&path)?;
            fs2::FileExt::lock_exclusive(&file).context("lock the move")?;
            Ok(Self { path, _file: file })
        })
        .await?
    }

    /// The lock on the move of the space `id`, or none while another
    /// process holds it.
    pub fn try_take(paths: &Paths, id: &SpaceId) -> Result<Option<Self>> {
        let path = lock_path(paths, id)?;
        let file = open(&path)?;
        Ok(match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Some(Self { path, _file: file }),
            Err(_) => None,
        })
    }

    /// Release the lock of a move that ended, deleting its file.
    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The lock file of the move of the space `id`, in a directory that exists.
fn lock_path(paths: &Paths, id: &SpaceId) -> Result<PathBuf> {
    let directory = paths.move_packages();
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create {}", directory.display()))?;
    Ok(directory.join(format!("{id}.lock")))
}

fn open(path: &std::path::Path) -> Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests;
