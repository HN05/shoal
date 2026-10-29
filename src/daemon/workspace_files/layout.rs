//! Where a repository keeps the files attached to its workspaces: beside the
//! workspaces, in a directory its agents are given as a working directory, so
//! they read the files where any sandbox lets them; they stay until their
//! workspace is removed.
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use crate::{
    daemon::file_copies::{incomplete_copies, keep_copies},
    forge::items::attach::Attachment,
    fsutil::{create_marked_dir, is_marked_dir},
    model::Repository,
};

/// The directory below a repository's attached files that keeps what
/// workspaces handed back, which no workspace ID names.
const RESULTS: &str = "results";

/// The largest file a message may attach.
pub(crate) const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// The directory of one repository's attached files.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub(crate) fn new(workspaces_dir: &Path, repository_id: &str) -> Self {
        Self {
            root: workspaces_dir.join(".attachments"),
            owner: repository_id.to_owned(),
        }
    }

    /// The repository's attached files; registrations from before the
    /// repository directory layout have none.
    pub(crate) fn of(repo: &Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    /// Copies of `files`, attached to `message` for the workspace
    /// `workspace_id`. The same message with the same files reuses its
    /// copies, so sending it again collapses with the unread one.
    pub(crate) fn keep(
        &self,
        workspace_id: &str,
        message: &str,
        files: &[PathBuf],
    ) -> Result<Kept> {
        self.keep_in(&self.root.join(workspace_id), workspace_id, message, files)
    }

    /// Copies of the files `files` that the workspace `workspace_id` hands
    /// back with its completion, apart from its other files since they
    /// outlive its removal.
    pub(crate) fn keep_results(&self, workspace_id: &str, files: &[PathBuf]) -> Result<Kept> {
        let directory = self.root.join(RESULTS).join(workspace_id);
        self.keep_in(&directory, workspace_id, "shoal done", files)
    }

    /// Copies of `files` in `directory`, for the workspace `workspace_id`.
    fn keep_in(
        &self,
        directory: &Path,
        workspace_id: &str,
        message: &str,
        files: &[PathBuf],
    ) -> Result<Kept> {
        for file in files {
            check(file)?;
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
            .map(|path| Attachment {
                path: path.clone(),
                alt: None,
            })
            .collect();
        // A copy someone deleted is made again with the others.
        if let Some(stale) = incomplete_copies(directory, workspace_id, message, &attachments)? {
            remove_dir(&stale)?;
        }
        let copies = keep_copies(directory, workspace_id, message, &attachments)?;
        let paths: Vec<_> = copies
            .attachments
            .into_iter()
            .map(|copy| copy.path)
            .collect();
        let bytes = paths
            .iter()
            .map(|path| {
                let metadata =
                    std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
                Ok(metadata.len())
            })
            .collect::<Result<_>>()?;
        Ok(Kept {
            paths,
            bytes,
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
    pub(crate) fn workspace_copies(&self, workspace_id: &str) -> Result<Vec<PathBuf>> {
        if !is_marked_dir(&self.root, &self.owner) {
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
    pub(crate) fn remove_workspace(&self, workspace_id: &str) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root.join(workspace_id))
    }

    /// The directory of the files the workspace `workspace_id` handed back.
    pub(crate) fn results(&self, workspace_id: &str) -> PathBuf {
        self.root.join(RESULTS).join(workspace_id)
    }

    /// Delete the files the workspace `workspace_id` handed back.
    pub(crate) fn remove_results(&self, workspace_id: &str) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        let results = self.results(workspace_id);
        ensure!(
            self.contains(&results),
            "{} is not among the repository's attached files",
            results.display()
        );
        remove_dir(&results)
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

/// The copies [`Layout::keep`] made or reused for one message.
#[derive(Debug)]
pub(crate) struct Kept {
    pub(crate) paths: Vec<PathBuf>,
    /// The size of each copy in `paths`, in bytes.
    pub(crate) bytes: Vec<u64>,
    /// The directory of copies made for this message, rather than reused.
    created: Option<PathBuf>,
    /// The directory of the workspace's attached files.
    workspace: PathBuf,
}

impl Kept {
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

/// Refuse what a message cannot attach: a relative path, something other
/// than a file, or a file over [`MAX_FILE_BYTES`].
pub(crate) fn check(path: &Path) -> Result<()> {
    let shown = path.display();
    ensure!(path.is_absolute(), "{shown}: give an absolute path");
    let metadata = std::fs::metadata(path).with_context(|| format!("read {shown}"))?;
    ensure!(metadata.is_file(), "{shown} is not a file");
    ensure!(
        metadata.len() <= MAX_FILE_BYTES,
        "{shown}: attached files must be at most {} MiB",
        MAX_FILE_BYTES / 1024 / 1024
    );
    Ok(())
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
