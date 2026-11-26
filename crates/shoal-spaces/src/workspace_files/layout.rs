//! Where attached files are kept: beside a repository's workspaces, or in
//! `root_dir` for the chats without a repository. See
//! `docs/design/coordination.md`.
use shoal_ids::space_ids::SpaceId;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use super::NewFile;
use crate::{
    forge::items::attach::Attachment,
    model::Repository,
    shoal_dirs::{Contents, ShoalDir, remove_dir, remove_if_empty},
};
use shoal_daemon_base::file_copies::{incomplete_copies, keep_copies};
use shoal_protocol::files::SessionCopy;

/// Keeps what workspaces handed back. No workspace ID names it.
const RESULTS: &str = "results";

#[derive(Debug, Clone)]
pub struct Layout {
    dir: ShoalDir,
}

impl Layout {
    pub fn of(repo: &Repository) -> Self {
        Self {
            dir: ShoalDir::of(repo, Contents::Attachments),
        }
    }

    /// Marked as those of the daemon whose state directory is `state`.
    pub fn outside_repositories(root_dir: &Path, state: &Path) -> Self {
        Self {
            dir: ShoalDir::outside_repositories(root_dir, state, Contents::Attachments),
        }
    }

    fn root(&self) -> &Path {
        self.dir.root()
    }

    /// None unless this Shoal created the directory for its owner.
    pub fn own_root(&self) -> Option<&Path> {
        self.dir.is_own().then(|| self.root())
    }

    pub fn existing(&self, id: &SpaceId) -> Option<PathBuf> {
        let directory = self.root().join(id);
        (self.dir.is_own() && directory.is_dir()).then_some(directory)
    }

    /// Creates the marked directory when it is missing.
    pub fn prepare(&self, id: &SpaceId) -> Result<PathBuf> {
        self.dir.create()?;
        let directory = self.root().join(id);
        std::fs::create_dir(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        Ok(directory)
    }

    /// The same message with the same files reuses its copies, so sending it
    /// again collapses with the unread one.
    pub fn keep(&self, workspace_id: &SpaceId, message: &str, files: &[NewFile]) -> Result<Kept> {
        let directory = self.root().join(workspace_id);
        self.keep_in(&directory, workspace_id, message, files, Limit::Sent)
    }

    /// Copies whatever their size: a handoff that dropped one would lose the
    /// history it holds.
    pub fn keep_session(
        &self,
        space_id: &SpaceId,
        message: &str,
        files: &[(NewFile, SessionCopy)],
    ) -> Result<Kept> {
        let directory = self.root().join(space_id);
        let new: Vec<NewFile> = files.iter().map(|(file, _)| file.clone()).collect();
        let mut kept = self.keep_in(&directory, space_id, message, &new, Limit::None)?;
        for (copy, (_, session)) in kept.copies.iter_mut().zip(files) {
            copy.session = Some(session.clone());
        }
        Ok(kept)
    }

    /// Such as with its completion or an alert.
    pub fn keep_results(
        &self,
        workspace_id: &SpaceId,
        message: &str,
        files: &[NewFile],
    ) -> Result<Kept> {
        let directory = self.root().join(RESULTS).join(workspace_id);
        self.keep_in(&directory, workspace_id, message, files, Limit::Sent)
    }

    /// Refuses each file as `limit` says.
    fn keep_in(
        &self,
        directory: &Path,
        workspace_id: &SpaceId,
        message: &str,
        files: &[NewFile],
        limit: Limit,
    ) -> Result<Kept> {
        for file in files {
            limit.check(&file.path)?;
            file.check()?;
        }
        self.dir.create()?;
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
                    session: None,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Kept {
            copies: kept,
            created: copies.created,
            workspace: directory.to_owned(),
        })
    }

    /// Also deletes the directories that held only it. Refuses a path that
    /// is not inside this repository's attached files, or leaves them
    /// through a symbolic link.
    pub fn delete_copy(&self, path: &Path) -> Result<()> {
        self.dir.ensure_own()?;
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
        // A copy sits in a directory of its own, below the files attached
        // with it, below those of its space's ID, which each go once empty.
        for directory in path.ancestors().skip(1).take(3) {
            if std::fs::remove_dir(directory).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// Its own directory, and the results it handed back.
    pub fn kept(&self, space_id: &SpaceId) -> [PathBuf; 2] {
        [
            self.root().join(space_id),
            self.root().join(RESULTS).join(space_id),
        ]
    }

    /// Also deletes the `copies` its records name elsewhere, as a restored
    /// workspace keeps those of the ID it had before.
    pub fn remove_kept(&self, space_id: &SpaceId, copies: &[PathBuf]) -> Result<()> {
        if !self.dir.is_own() {
            return Ok(());
        }
        copies.iter().try_for_each(|copy| self.delete_copy(copy))?;
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

    /// Its directory must stay there through any symbolic link. A directory
    /// that is gone holds nothing to delete.
    fn contains(&self, path: &Path) -> bool {
        path.starts_with(self.root())
            && path
                .parent()
                .is_some_and(|parent| match parent.canonicalize() {
                    Ok(_) => self.resolves_inside(parent),
                    Err(error) => error.kind() == ErrorKind::NotFound,
                })
    }

    fn resolves_inside(&self, directory: &Path) -> bool {
        match (directory.canonicalize(), self.root().canonicalize()) {
            (Ok(directory), Ok(root)) => directory.starts_with(root),
            _ => false,
        }
    }

    /// Runs when the repository goes.
    pub fn remove(&self) -> Result<()> {
        self.dir.remove()
    }
}

#[derive(Clone, Copy)]
enum Limit {
    /// What a message cannot send, as [`super::check_path`] refuses it.
    Sent,
    /// Anything but an absolute path to a file.
    None,
}

impl Limit {
    fn check(self, path: &Path) -> Result<()> {
        match self {
            Self::Sent => super::check_path(path),
            Self::None => {
                let shown = path.display();
                ensure!(path.is_absolute(), "{shown}: give an absolute path");
                ensure!(path.is_file(), "{shown} is not a file");
                Ok(())
            }
        }
    }
}

/// As its record keeps it.
#[derive(Debug, Clone)]
pub struct Copy {
    pub path: PathBuf,
    pub bytes: u64,
    pub title: Option<String>,
    /// Set for a handoff's copy.
    pub session: Option<shoal_protocol::files::SessionCopy>,
}

#[derive(Debug, Clone)]
pub struct Kept {
    pub copies: Vec<Copy>,
    /// None when the copies were reused.
    created: Option<PathBuf>,
    workspace: PathBuf,
}

impl Kept {
    pub fn reused(&self) -> bool {
        self.created.is_none()
    }

    /// For a message that was not queued, as when the workspace was removed
    /// while the files were copied. Also deletes the workspace's directory
    /// when nothing else is left in it.
    pub fn discard(self) -> Result<()> {
        if let Some(created) = &self.created {
            remove_dir(created)?;
        }
        remove_if_empty(&self.workspace)
    }
}
