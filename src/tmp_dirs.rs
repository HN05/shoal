//! Temporary directories: every workspace and chat of a repository gets its
//! own in the repository directory's `.tmp`, and every chat without a
//! repository in `root_dir`'s, named by its ID, which its processes receive
//! as `TMPDIR`. The directory goes with its workspace or chat, so nothing in
//! it is ever kept.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    fsutil::marked_dirs::{create_marked_dir, is_marked_dir, marked_owner},
    model::Repository,
};

/// The directory of one repository's temporary directories, or of those of
/// the chats without a repository.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub fn new(workspaces_dir: &Path, repository_id: &RepositoryId) -> Self {
        Self::marked(workspaces_dir, repository_id.to_string())
    }

    /// The temporary directories in `directory`, marked as `owner`'s.
    fn marked(directory: &Path, owner: String) -> Self {
        Self {
            root: directory.join(".tmp"),
            owner,
        }
    }

    /// The temporary directories of the chats without a repository, in
    /// `root_dir`, marked as those of the daemon whose state directory is
    /// `state`, which no repository ID names: a daemon sharing `root_dir`
    /// neither uses nor sweeps them.
    pub fn outside_repositories(root_dir: &Path, state: &Path) -> Self {
        Self::marked(root_dir, state.display().to_string())
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
    pub fn locate(&self, id: &SpaceId) -> PathBuf {
        self.root.join(id)
    }

    /// The temporary directory of the workspace or chat `id`, created private
    /// to the user when missing.
    pub fn prepare(&self, id: &SpaceId) -> Result<PathBuf> {
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        self.ensure_own()?;
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

    /// Refuse a directory in the way that Shoal did not create for this
    /// owner, leaving one another owner's, such as a daemon's that shares
    /// `root_dir`, to it.
    fn ensure_own(&self) -> Result<()> {
        if let Some(owner) = marked_owner(&self.root).filter(|owner| *owner != self.owner) {
            bail!(
                "{} holds the temporary directories of {owner}, which this Shoal is not; give each \
                 Shoal its own root_dir",
                self.root.display()
            );
        }
        ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a directory Shoal created for temporary files; move it away",
            self.root.display()
        );
        Ok(())
    }

    /// The temporary directory of the workspace or chat `id` when it is one
    /// Shoal created: a directory, not a link, in the directory marked for
    /// this repository. What works in it counts as the space's, so a link
    /// put in its place must not lead elsewhere.
    pub fn existing(&self, id: &SpaceId) -> Option<PathBuf> {
        let directory = self.locate(id);
        (is_marked_dir(&self.root, &self.owner)
            && fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()))
        .then_some(directory)
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
    pub fn remove_one(&self, id: &SpaceId) -> Result<()> {
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
pub fn locate(repo: &Repository, id: &SpaceId) -> Option<PathBuf> {
    Layout::of(repo).map(|layout| layout.locate(id))
}

/// The temporary directory of the repository's workspace or chat `id`,
/// created when missing; `None` without a repository directory.
pub fn directory(repo: &Repository, id: &SpaceId) -> Result<Option<PathBuf>> {
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
