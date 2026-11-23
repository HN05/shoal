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

/// Delete the copies in `dir` last written before `cutoff`, and the
/// directories of spaces they leave empty, keeping `dir` and its mark, which
/// a fetch under way writes below; whether any went. A directory Shoal did
/// not create stays.
pub fn remove_written_before(dir: &ShoalDir, cutoff: SystemTime) -> Result<bool> {
    remove_copies(dir, |path| Ok(last_written(path)? < cutoff))
}

/// Delete the copies in `dir` that lose nothing, as [`remove_written_before`]
/// deletes: each a fetch finished writing and nobody changed since, which
/// the machine that keeps its source can give again.
pub fn remove_unchanged(dir: &ShoalDir) -> Result<bool> {
    remove_copies(dir, |path| {
        Ok(is_real_dir(path)?
            && is_as_fetched(path).with_context(|| format!("read {}", path.display()))?)
    })
}

/// Delete the copies in `dir` that `goes` selects, with the directories of
/// spaces they leave empty, keeping `dir` and its mark; whether any went.
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

/// Delete `path`, a copy's directory or anything else left there, when
/// `goes` selects it; whether it did.
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

/// When `path`, or anything directly in it, last changed, as a copy's
/// directory changes when a fetch writes into it and its file when the user
/// edits it; long ago for what is already gone.
fn last_written(path: &Path) -> Result<SystemTime> {
    let mut latest = modified(path)?;
    if is_real_dir(path)? {
        for entry in entries(path)? {
            latest = latest.max(modified(&entry)?);
        }
    }
    Ok(latest)
}

/// When `path` itself, never what a symbolic link names, last changed.
fn modified(path: &Path) -> Result<SystemTime> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata
            .modified()
            .with_context(|| format!("read {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(UNIX_EPOCH),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Whether `path` is a directory, never through a symbolic link.
fn is_real_dir(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The paths in `directory`; none once it is gone.
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
