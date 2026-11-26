//! The fetched copies in one directory of them: `<space id>/<file id>/`,
//! each holding the file a fetch wrote, or the part it is writing.
use std::{
    io::ErrorKind,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};

use crate::{
    fetched_copies::is_as_fetched,
    fsutil::marked_dirs::OWNER_MARKER,
    shoal_dirs::{ShoalDir, remove_dir, remove_file, remove_if_empty},
};

/// Also deletes the space directories they leave empty. Keeps `dir` and its
/// mark, which a fetch under way writes below. A directory Shoal did not
/// create stays. Returns whether any went.
pub fn remove_written_before(dir: &ShoalDir, cutoff: SystemTime) -> Result<bool> {
    remove_copies(dir, |path| Ok(last_written(path)? < cutoff))
}

/// Deletes the copies a fetch finished writing and nobody changed since,
/// which the machine that keeps their source can give again.
pub fn remove_unchanged(dir: &ShoalDir) -> Result<bool> {
    remove_copies(dir, |path| {
        Ok(is_real_dir(path)?
            && is_as_fetched(path).with_context(|| format!("read {}", path.display()))?)
    })
}

/// Keeps `dir` and its mark. Returns whether any went.
fn remove_copies(dir: &ShoalDir, goes: impl Fn(&Path) -> Result<bool>) -> Result<bool> {
    if !dir.is_own() {
        return Ok(false);
    }
    let mut removed = false;
    for space in entries(dir.root())? {
        if space.file_name() == Some(OWNER_MARKER.as_ref()) {
            continue;
        }
        if !is_real_dir(&space)? {
            removed |= remove_if(&space, &goes)?;
            continue;
        }
        for copy in entries(&space)? {
            removed |= remove_if(&copy, &goes)?;
        }
        remove_if_empty(&space)?;
    }
    Ok(removed)
}

/// Returns whether it did.
fn remove_if(path: &Path, goes: impl Fn(&Path) -> Result<bool>) -> Result<bool> {
    if !goes(path)? {
        return Ok(false);
    }
    match is_real_dir(path)? {
        true => remove_dir(path)?,
        false => remove_file(path)?,
    }
    Ok(true)
}

/// A copy's directory changes when a fetch writes into it, and its file when
/// the user edits it. Long ago for what is already gone.
fn last_written(path: &Path) -> Result<SystemTime> {
    let mut latest = modified(path)?;
    if is_real_dir(path)? {
        for entry in entries(path)? {
            latest = latest.max(modified(&entry)?);
        }
    }
    Ok(latest)
}

/// Never what a symbolic link names.
fn modified(path: &Path) -> Result<SystemTime> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata
            .modified()
            .with_context(|| format!("read {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(UNIX_EPOCH),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Never through a symbolic link.
fn is_real_dir(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// None once the directory is gone.
fn entries(directory: &Path) -> Result<Vec<std::path::PathBuf>> {
    let read = match std::fs::read_dir(directory) {
        Ok(read) => read,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    read.map(|entry| Ok(entry?.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("read {}", directory.display()))
}

#[cfg(test)]
mod tests;
