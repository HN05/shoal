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
    daemon::file_copies::keep_copies,
    forge::items::attach::Attachment,
    fsutil::{create_marked_dir, is_marked_dir},
    model::Repository,
};

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
        let directory = self.root.join(workspace_id);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let attachments: Vec<_> = files
            .iter()
            .map(|path| Attachment {
                path: path.clone(),
                alt: None,
            })
            .collect();
        let copies = keep_copies(&directory, workspace_id, message, &attachments)?;
        Ok(Kept {
            paths: copies
                .attachments
                .into_iter()
                .map(|copy| copy.path)
                .collect(),
            created: copies.created,
            workspace: directory,
        })
    }

    /// Delete the files attached for the workspace `workspace_id`.
    pub(crate) fn remove_workspace(&self, workspace_id: &str) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root.join(workspace_id))
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

fn remove_dir(directory: &Path) -> Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}
