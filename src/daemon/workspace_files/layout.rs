//! Where a repository keeps the files attached to its workspaces and chats:
//! beside the workspaces, in a directory its agents are given as a working
//! directory, so they read the files where any sandbox lets them. The chats
//! without a repository keep theirs in `root_dir`, where they run.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use super::NewFile;
use crate::{
    daemon::file_copies::{incomplete_copies, keep_copies},
    forge::items::attach::Attachment,
    fsutil::marked_dirs::{check_marked_dir, create_marked_dir, is_marked_dir},
    model::Repository,
};

/// The directory below a repository's attached files that keeps what
/// workspaces handed back, which no workspace ID names.
const RESULTS: &str = "results";

/// The directory of one repository's attached files.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub(crate) fn new(workspaces_dir: &Path, repository_id: &RepositoryId) -> Self {
        Self::marked(workspaces_dir, repository_id.to_string())
    }

    /// The attached files in `directory`, marked as `owner`'s.
    fn marked(directory: &Path, owner: String) -> Self {
        Self {
            root: directory.join(".attachments"),
            owner,
        }
    }

    /// The repository's attached files; registrations from before the
    /// repository directory layout have none.
    pub(crate) fn of(repo: &Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    /// The attached files of the chats without a repository, in
    /// `root_dir`, marked as those of the daemon whose state directory is
    /// `state`, which no repository ID names: a daemon sharing `root_dir`
    /// neither uses nor sweeps them.
    pub(crate) fn outside_repositories(root_dir: &Path, state: &Path) -> Self {
        Self::marked(root_dir, state.display().to_string())
    }

    /// Copies of `files`, attached to `message` for the workspace
    /// `workspace_id`. The same message with the same files reuses its
    /// copies, so sending it again collapses with the unread one.
    pub(crate) fn keep(
        &self,
        workspace_id: &SpaceId,
        message: &str,
        files: &[NewFile],
    ) -> Result<Kept> {
        self.keep_in(&self.root.join(workspace_id), workspace_id, message, files)
    }

    /// Copies of the files `files` that the workspace `workspace_id` hands
    /// the user with `message`, such as its completion or an alert, apart
    /// from its other files since they outlive its removal.
    pub(crate) fn keep_results(
        &self,
        workspace_id: &SpaceId,
        message: &str,
        files: &[NewFile],
    ) -> Result<Kept> {
        let directory = self.root.join(RESULTS).join(workspace_id);
        self.keep_in(&directory, workspace_id, message, files)
    }

    /// Copies of `files` in `directory`, for the workspace `workspace_id`.
    fn keep_in(
        &self,
        directory: &Path,
        workspace_id: &SpaceId,
        message: &str,
        files: &[NewFile],
    ) -> Result<Kept> {
        for file in files {
            super::check_path(&file.path)?;
            file.check()?;
        }
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a directory Shoal created for attached files; move it away",
            self.root.display()
        );
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create {}", directory.display()))?;
        ensure!(
            self.resolves_inside(directory),
            "{} is not among the repository's attached files",
            directory.display()
        );
        let attachments: Vec<_> = files
            .iter()
            .map(|file| Attachment {
                path: file.path.clone(),
                alt: None,
            })
            .collect();
        // A copy someone deleted is made again with the others.
        if let Some(stale) = incomplete_copies(directory, workspace_id, message, &attachments)? {
            remove_dir(&stale)?;
        }
        let copies = keep_copies(directory, workspace_id, message, &attachments)?;
        let kept = copies
            .attachments
            .into_iter()
            .zip(files)
            .map(|(copy, file)| {
                let metadata = std::fs::metadata(&copy.path)
                    .with_context(|| format!("read {}", copy.path.display()))?;
                Ok(Copy {
                    path: copy.path,
                    bytes: metadata.len(),
                    title: file.title.clone(),
                })
            })
            .collect::<Result<_>>()?;
        Ok(Kept {
            copies: kept,
            created: copies.created,
            workspace: directory.to_owned(),
        })
    }

    /// Delete Shoal's copy at `path`, and the directories that held only it,
    /// once the path is inside this repository's attached files, which Shoal
    /// created, and stays there through any symbolic link.
    pub(crate) fn delete_copy(&self, path: &Path) -> Result<()> {
        ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a directory Shoal created for attached files",
            self.root.display()
        );
        let shown = path.display();
        ensure!(
            self.contains(path),
            "{shown} is not among the repository's attached files"
        );
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("delete {shown}"));
            }
            _ => {}
        }
        // A copy sits in a directory of its own, below the files attached with it.
        for directory in path.ancestors().skip(1).take(2) {
            if std::fs::remove_dir(directory).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// The copies kept for the workspace `workspace_id` outside its results:
    /// the files two directories below its own, one directory per message and
    /// one per file within it. A hidden message directory holds copies still
    /// being made; the files themselves may have any name.
    pub(crate) fn workspace_copies(&self, workspace_id: &SpaceId) -> Result<Vec<PathBuf>> {
        let ours = check_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("read {}", self.root.display()))?;
        if !ours {
            return Ok(Vec::new());
        }
        let staging = |path: &PathBuf| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        };
        let mut copies = Vec::new();
        for message in entries(&self.root.join(workspace_id))? {
            if !message.is_dir() || staging(&message) {
                continue;
            }
            for file in entries(&message)? {
                if file.is_dir() {
                    copies.extend(entries(&file)?.into_iter().filter(|copy| copy.is_file()));
                }
            }
        }
        copies.sort();
        Ok(copies)
    }

    /// Delete the files attached for the workspace `workspace_id`.
    pub(crate) fn remove_workspace(&self, workspace_id: &SpaceId) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root.join(workspace_id))
    }

    /// The directories that may hold the files kept for the workspace or
    /// chat `space_id` after it is gone: a chat's own, and the results a
    /// workspace handed back.
    pub(crate) fn kept(&self, space_id: &SpaceId) -> [PathBuf; 2] {
        [
            self.root.join(space_id),
            self.root.join(RESULTS).join(space_id),
        ]
    }

    /// Delete the files kept for the space `space_id` after it
    /// is gone.
    pub(crate) fn remove_kept(&self, space_id: &SpaceId) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        for directory in self.kept(space_id) {
            ensure!(
                self.contains(&directory),
                "{} is not among the repository's attached files",
                directory.display()
            );
            remove_dir(&directory)?;
        }
        Ok(())
    }

    /// Whether `path` is below this repository's attached files and its
    /// directory stays there through any symbolic link; a directory that is
    /// gone holds nothing to delete.
    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
            && path
                .parent()
                .is_some_and(|parent| match parent.canonicalize() {
                    Ok(_) => self.resolves_inside(parent),
                    Err(error) => error.kind() == ErrorKind::NotFound,
                })
    }

    /// Whether the existing `directory` resolves inside this repository's
    /// attached files through any symbolic link.
    fn resolves_inside(&self, directory: &Path) -> bool {
        match (directory.canonicalize(), self.root.canonicalize()) {
            (Ok(directory), Ok(root)) => directory.starts_with(root),
            _ => false,
        }
    }

    /// Delete the directory with every attached file, when the repository
    /// goes.
    pub(crate) fn remove(&self) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root)
    }
}

/// One of Shoal's copies, with its size in bytes and the title it was
/// given, as its record keeps it.
#[derive(Debug, Clone)]
pub(crate) struct Copy {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) title: Option<String>,
}

/// The copies [`Layout::keep`] made or reused for one message.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    pub(crate) copies: Vec<Copy>,
    /// The directory of copies made for this message, rather than reused.
    created: Option<PathBuf>,
    /// The directory of the workspace's attached files.
    workspace: PathBuf,
}

impl Kept {
    /// Whether the copies were made for an identical message before.
    pub(crate) fn reused(&self) -> bool {
        self.created.is_none()
    }

    /// Delete the copies made for a message that was not queued, and the
    /// workspace's directory when nothing else is left in it, as when the
    /// workspace was removed while they were copied.
    pub(crate) fn discard(self) -> Result<()> {
        if let Some(created) = &self.created {
            remove_dir(created)?;
        }
        match std::fs::remove_dir(&self.workspace) {
            Err(error)
                if !matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Err(error).with_context(|| format!("delete {}", self.workspace.display()))
            }
            _ => Ok(()),
        }
    }
}

/// The entries of `directory`, none when it does not exist.
fn entries(directory: &Path) -> Result<Vec<PathBuf>> {
    let listing = match std::fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    listing
        .map(|entry| Ok(entry?.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("read {}", directory.display()))
}

fn remove_dir(directory: &Path) -> Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}
