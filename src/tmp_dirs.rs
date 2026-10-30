//! Temporary directories: every workspace and chat of a repository gets its
//! own in the repository directory's `.tmp`, named by its ID, which its
//! processes receive as `TMPDIR`. The directory goes with its workspace or
//! chat, so nothing in it is ever kept.
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use crate::{
    fsutil::{create_marked_dir, is_marked_dir},
    model::Repository,
};

/// The directory of one repository's temporary directories.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub fn new(workspaces_dir: &Path, repository_id: &str) -> Self {
        Self {
            root: workspaces_dir.join(".tmp"),
            owner: repository_id.to_owned(),
        }
    }

    /// The repository's temporary directories; registrations from before the
    /// repository directory layout have none.
    pub fn of(repo: &Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The temporary directory of the workspace or chat `id`, without
    /// creating it.
    pub fn locate(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// The temporary directory of the workspace or chat `id`, created private
    /// to the user when missing.
    pub fn prepare(&self, id: &str) -> Result<PathBuf> {
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a directory Shoal created for temporary files; move it away",
            self.root.display()
        );
        let directory = self.locate(id);
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                return Err(error).with_context(|| format!("create {}", directory.display()));
            }
            _ => {}
        }
        ensure!(
            fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()),
            "{} is not a directory",
            directory.display()
        );
        Ok(directory)
    }

    /// The IDs whose temporary directories exist; none when the directory is
    /// not the one Shoal created for this repository.
    pub fn list(&self) -> Result<Vec<String>> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(Vec::new());
        }
        let entries =
            fs::read_dir(&self.root).with_context(|| format!("read {}", self.root.display()))?;
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Delete the temporary directory of the workspace or chat `id`.
    pub fn remove_one(&self, id: &str) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.locate(id))
    }

    /// Delete every temporary directory with the directory itself, when the
    /// repository goes; a directory Shoal did not create is the user's and
    /// stays.
    pub fn remove(&self) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root)
    }
}

/// The temporary directory of the repository's workspace or chat `id`,
/// without creating it; `None` without a repository directory.
pub fn locate(repo: &Repository, id: &str) -> Option<PathBuf> {
    Layout::of(repo).map(|layout| layout.locate(id))
}

/// The temporary directory of the repository's workspace or chat `id`,
/// created when missing; `None` without a repository directory.
pub fn directory(repo: &Repository, id: &str) -> Result<Option<PathBuf>> {
    Layout::of(repo)
        .map(|layout| layout.prepare(id))
        .transpose()
}

fn remove_dir(directory: &Path) -> Result<()> {
    match fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
