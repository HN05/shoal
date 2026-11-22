//! The directories Shoal keeps beside workspaces. Each is in its repository's
//! directory, marked with the repository's ID, or for what belongs to no
//! repository in `root_dir`, marked with the state directory of the daemon
//! that keeps it, so a daemon sharing `root_dir` neither uses nor sweeps
//! another's.
use shoal_ids::repository_ids::RepositoryId;
use std::{
    fs::Permissions,
    io::{self, ErrorKind},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    fsutil::marked_dirs::{
        OWNER_MARKER, check_marked_dir, create_marked_dir, is_marked_dir, marked_owner,
    },
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
    /// Each running local review's checkout and its reviewer's output.
    ReviewRuns,
    /// Each running gate decision's schema and output.
    GateRuns,
    /// The tool caches every workspace of a repository shares.
    Caches,
    /// The agents' session files an import carried, until a restore puts
    /// them in place.
    ImportedSessions,
    /// The packages imports are reading, until they record their spaces.
    Imports,
    /// The seed build new workspaces start from.
    Seed,
}

impl Contents {
    pub const ALL: [Self; 9] = [
        Self::TmpDirs,
        Self::Attachments,
        Self::ProposalFiles,
        Self::ReviewRuns,
        Self::GateRuns,
        Self::Caches,
        Self::ImportedSessions,
        Self::Imports,
        Self::Seed,
    ];

    /// The directory's name beside the workspaces.
    pub fn directory(self) -> &'static str {
        match self {
            Self::TmpDirs => ".tmp",
            Self::Attachments => ".attachments",
            Self::ProposalFiles => ".proposals",
            Self::ReviewRuns => ".reviews",
            Self::GateRuns => ".gate",
            Self::Caches => ".caches",
            Self::ImportedSessions => ".sessions",
            Self::Imports => ".imports",
            Self::Seed => ".seed",
        }
    }

    /// Whether the directory may hold directories closed to its owner,
    /// such as what any process leaves in a temporary directory or a
    /// review's read-only checkout, so that deleting it opens them.
    fn may_hold_closed_dirs(self) -> bool {
        matches!(self, Self::TmpDirs | Self::ReviewRuns)
    }

    fn description(self) -> &'static str {
        match self {
            Self::TmpDirs => "temporary files",
            Self::Attachments => "attached files",
            Self::ProposalFiles => "proposals' files",
            Self::ReviewRuns => "review runs",
            Self::GateRuns => "gate runs",
            Self::Caches => "shared caches",
            Self::ImportedSessions => "imported session files",
            Self::Imports => "packages being imported",
            Self::Seed => "seed build",
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

    /// The repository's directory of `contents`.
    pub fn of(repo: &Repository, contents: Contents) -> Self {
        Self::new(&repo.workspaces_dir, &repo.id, contents)
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

    /// The repository ID, or state directory, the directory is marked as.
    pub fn owner(&self) -> &str {
        &self.owner
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
        match self.contents.may_hold_closed_dirs() {
            true => remove_dir_opening(&self.root),
            false => remove_dir(&self.root),
        }
    }

    /// Delete the directory's mark, then the directory when nothing else is
    /// left in it; what is left is no longer Shoal's.
    pub fn unmark(&self) -> Result<()> {
        remove_file(&self.root.join(OWNER_MARKER))?;
        remove_if_empty(&self.root)
    }
}

/// Delete `directory` with everything in it, when it exists.
pub fn remove_dir(directory: &Path) -> Result<()> {
    deleted(directory, std::fs::remove_dir_all(directory))
}

/// [`remove_dir`] for a directory that may hold directories closed to the
/// owner, such as a review's read-only checkout a test left in its
/// temporary directory: they are opened to the owner first.
pub fn remove_dir_opening(directory: &Path) -> Result<()> {
    deleted(directory, remove_opening(directory))
}

/// The outcome of deleting `directory`, which may already be gone.
fn deleted(directory: &Path, result: io::Result<()>) -> Result<()> {
    match result {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

/// Delete `directory` with everything in it, opening the directories in it
/// to the owner when their permissions refuse that.
fn remove_opening(directory: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            open_to_owner(directory)?;
            std::fs::remove_dir_all(directory)
        }
        result => result,
    }
}

/// Give the owner read, write and search permission on `directory` and on
/// every directory below it, without following symbolic links.
fn open_to_owner(directory: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(directory, Permissions::from_mode(mode | 0o700))?;
    }
    for entry in std::fs::read_dir(directory)? {
        open_to_owner(&entry?.path())?;
    }
    Ok(())
}

/// Delete `file` when it exists.
pub fn remove_file(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", file.display()))
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
