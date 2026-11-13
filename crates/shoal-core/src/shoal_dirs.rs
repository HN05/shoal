//! The directories Shoal keeps beside workspaces for files it gives the
//! user, agents and reviewers to work with. Each is in its repository's
//! directory, marked with the repository's ID, or for what belongs to no
//! repository in `root_dir`, marked with the state directory of the daemon
//! that keeps it, so a daemon sharing `root_dir` neither uses nor sweeps
//! another's.
use crate::client::repository_ids::RepositoryId;
use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    fsutil::marked_dirs::{check_marked_dir, create_marked_dir, is_marked_dir, marked_owner},
    model::Repository,
};

/// What a directory Shoal keeps beside workspaces holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contents {
    /// Each workspace's and chat's temporary directory.
    TmpDirs,
    /// The files attached to workspaces and chats.
    Attachments,
    /// Copies of the files proposed issues and PRs attach.
    ProposalFiles,
}

impl Contents {
    pub const ALL: [Self; 3] = [Self::TmpDirs, Self::Attachments, Self::ProposalFiles];

    fn directory(self) -> &'static str {
        match self {
            Self::TmpDirs => ".tmp",
            Self::Attachments => ".attachments",
            Self::ProposalFiles => ".proposals",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::TmpDirs => "temporary files",
            Self::Attachments => "attached files",
            Self::ProposalFiles => "proposals' files",
        }
    }
}

/// One directory Shoal keeps beside workspaces, with the owner it is marked
/// as.
#[derive(Debug, Clone)]
pub struct ShoalDir {
    root: PathBuf,
    owner: String,
    contents: Contents,
}

impl ShoalDir {
    /// The directory of `contents` in the repository directory
    /// `workspaces_dir`.
    pub fn new(workspaces_dir: &Path, repository_id: &RepositoryId, contents: Contents) -> Self {
        Self::marked(workspaces_dir, repository_id.to_string(), contents)
    }

    /// The directory of `contents` that belong to no repository, in
    /// `root_dir`, marked as those of the daemon whose state directory is
    /// `state`, which no repository ID names.
    pub fn outside_repositories(root_dir: &Path, state: &Path, contents: Contents) -> Self {
        Self::marked(root_dir, state.display().to_string(), contents)
    }

    /// The repository's directory of `contents`; registrations from before
    /// the repository directory layout have none.
    pub fn of(repo: &Repository, contents: Contents) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id, contents))
    }

    fn marked(directory: &Path, owner: String, contents: Contents) -> Self {
        Self {
            root: directory.join(contents.directory()),
            owner,
            contents,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the directory marked as its owner's when it is missing, and
    /// refuse one in the way that Shoal did not create for this owner.
    pub fn create(&self) -> Result<()> {
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        self.ensure_own()
    }

    /// Refuse a directory that Shoal did not create for this owner, leaving
    /// one another owner's, such as a daemon's that shares `root_dir`, to it.
    pub fn ensure_own(&self) -> Result<()> {
        let shown = self.root.display();
        let holds = self.contents.description();
        if let Some(owner) = marked_owner(&self.root).filter(|owner| *owner != self.owner) {
            bail!(
                "{shown} holds the {holds} of {owner}, which this Shoal is not; give each Shoal \
                 its own root_dir"
            );
        }
        ensure!(
            self.is_own(),
            "{shown} is not a directory Shoal created for {holds}; move it away"
        );
        Ok(())
    }

    /// Whether the directory is a real one Shoal created for this owner, so
    /// nothing created or deleted in it is reached through a symlink or
    /// belongs to anyone else.
    pub fn is_own(&self) -> bool {
        is_marked_dir(&self.root, &self.owner)
    }

    /// [`Self::is_own`], failing when the directory or its mark exists but
    /// cannot be read.
    pub fn check_own(&self) -> io::Result<bool> {
        check_marked_dir(&self.root, &self.owner)
    }

    /// Delete the directory with everything in it, when its owner goes; a
    /// directory Shoal did not create is the user's and stays.
    pub fn remove(&self) -> Result<()> {
        if !self.is_own() {
            return Ok(());
        }
        remove_dir(&self.root)
    }
}

/// Delete `directory` with everything in it, when it exists.
pub fn remove_dir(directory: &Path) -> Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

/// Delete `directory` when it is empty; anything left in it stays.
pub fn remove_if_empty(directory: &Path) -> Result<()> {
    match std::fs::remove_dir(directory) {
        Err(error)
            if !matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
