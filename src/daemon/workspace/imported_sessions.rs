//! The agents' session files an import carried, kept with the removal
//! record it made until the restore that opens the workspace puts them in
//! place with the workspace's own settings. A repository keeps them in its
//! directory's `.sessions`, a directory per workspace named by its ID,
//! holding each file under its place in the list beside them. Files a
//! restore hands over but its caller does not put in place stay listed
//! there for the workspace's next removal record.
use crate::client::repository_ids::RepositoryId;
use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

use super::Manager;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    fsutil::marked_dirs::{create_marked_dir, is_marked_dir},
    model::{RemovedWorkspace, Repository, Workspace},
    transfer::{CarriedSessionFile, SessionFile, SessionSource},
};

impl Manager {
    /// Keep `files` for `removed`, imported into `repo`, replacing what an
    /// earlier import of it kept, and list them in its record. The caller
    /// holds the repository's Git gate and the checkpoint gate, so the sweep
    /// cannot take them before the record exists.
    pub(super) async fn keep_imported_sessions(
        &self,
        repo: &Repository,
        mut removed: RemovedWorkspace,
        files: &[CarriedSessionFile],
    ) -> Result<RemovedWorkspace> {
        removed.session_files = files.iter().map(|carried| carried.file.clone()).collect();
        let layout = match files.is_empty() {
            true => Layout::of(repo),
            false => Some(Layout::new(&self.workspaces_dir(repo).await?, &repo.id)),
        };
        let Some(layout) = layout else {
            return Ok(removed);
        };
        let copies: Vec<_> = files
            .iter()
            .zip(layout.kept(&removed.workspace_id, &removed.session_files))
            .map(|(carried, kept)| (carried.contents.clone(), kept.contents))
            .collect();
        let (workspace_id, listed) = (removed.workspace_id.clone(), removed.session_files.clone());
        tokio::task::spawn_blocking(move || layout.keep(&workspace_id, &listed, &copies))
            .await
            .context("keep the agents' session files")??;
        Ok(removed)
    }

    /// Forget the session files `repo` keeps for the workspace
    /// `workspace_id`.
    pub(super) async fn discard_imported_sessions(
        &self,
        repo: &Repository,
        workspace_id: &SpaceId,
    ) -> Result<()> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        let workspace_id = workspace_id.clone();
        tokio::task::spawn_blocking(move || layout.remove_one(&workspace_id))
            .await
            .context("delete the kept session files")?
    }

    /// Forget the session files the restore that opened `workspace` handed
    /// over, once the caller has put them in place.
    pub async fn drop_restored_sessions(&self, workspace: &SpaceSelector) -> Result<()> {
        let workspace = self.workspace(workspace).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        self.discard_imported_sessions(&repo, &workspace.id).await
    }

    /// Give the session files kept for `removed` to the workspace `to` that
    /// restores it, for the caller to put in place, returning them there.
    /// The checkpoint gate keeps the sweep from reading the files' owners
    /// before the move and deleting them after it.
    pub(super) async fn hand_over_imported_sessions(
        &self,
        removed: &RemovedWorkspace,
        to: &SpaceId,
    ) -> Result<Vec<CarriedSessionFile>> {
        if removed.session_files.is_empty() {
            return Ok(Vec::new());
        }
        let repo = self.repository(&removed.repository_id).await?;
        let layout = Layout::of(&repo).context("the repository has no directory")?;
        let kept = layout.kept(to, &removed.session_files);
        let (from, to) = (removed.workspace_id.clone(), to.clone());
        let _sweep = self.gates.lock_checkpoints().await;
        tokio::task::spawn_blocking(move || layout.hand_over(&from, &to))
            .await
            .context("hand over the kept session files")??;
        Ok(kept)
    }

    /// The session files kept for `workspace` that its restore's caller did
    /// not put in place, for its removal record. An unreadable list is not
    /// worth keeping the workspace for.
    pub(super) async fn unplaced_sessions(&self, workspace: &Workspace) -> Vec<SessionFile> {
        let Ok(repo) = self.repository(&workspace.repository_id).await else {
            return Vec::new();
        };
        let Some(layout) = Layout::of(&repo) else {
            return Vec::new();
        };
        let id = workspace.id.clone();
        match tokio::task::spawn_blocking(move || layout.listed(&id)).await {
            Ok(Ok(listed)) => listed,
            _ => Vec::new(),
        }
    }

    /// Delete the session files `repo` keeps for workspaces that neither
    /// exist nor have a removal record left. The caller holds the
    /// checkpoint gate.
    pub(super) async fn expire_imported_sessions(&self, repo: &Repository) -> Result<()> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        let owners = self.store.run(|db| session_owners(db)).await?;
        tokio::task::spawn_blocking(move || layout.remove_unowned(&owners))
            .await
            .context("delete expired session files")?
    }
}

/// The kept session files of `removed`, a workspace of `repo`, as sources
/// of its export.
pub(crate) fn kept_session_sources(
    repo: &Repository,
    removed: &RemovedWorkspace,
) -> Vec<SessionSource> {
    let Some(layout) = Layout::of(repo) else {
        return Vec::new();
    };
    layout
        .kept(&removed.workspace_id, &removed.session_files)
        .into_iter()
        .map(|kept| SessionSource {
            agent: kept.file.agent,
            session: kept.file.session,
            path: kept.file.path,
            source: kept.contents,
        })
        .collect()
}

/// Delete every kept session file of `repo` with their directory, when the
/// repository goes; a directory Shoal did not create is the user's and
/// stays.
pub(super) async fn remove_imported_sessions(repo: &Repository) -> Result<()> {
    let Some(layout) = Layout::of(repo) else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || layout.remove())
        .await
        .context("delete the kept session files")?
}

/// The file beside a workspace's kept session files that lists them.
const LIST: &str = "files.json";

/// The directory of one repository's kept session files.
pub(super) struct Layout {
    root: PathBuf,
    owner: RepositoryId,
}

impl Layout {
    pub(super) fn new(workspaces_dir: &Path, repository_id: &RepositoryId) -> Self {
        Self {
            root: workspaces_dir.join(".sessions"),
            owner: repository_id.to_owned(),
        }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The repository's kept session files; registrations from before the
    /// repository directory layout have none until an import reserves it.
    fn of(repo: &Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    /// The session files `files` kept for the workspace `id`, each named by
    /// its place among them.
    fn kept(&self, id: &SpaceId, files: &[SessionFile]) -> Vec<CarriedSessionFile> {
        let directory = self.root.join(id);
        files
            .iter()
            .enumerate()
            .map(|(index, file)| CarriedSessionFile {
                file: file.clone(),
                contents: directory.join(index.to_string()),
            })
            .collect()
    }

    /// Copy each source of `copies` to its destination in the directory of
    /// the workspace `id`, which starts empty, and is left out without any,
    /// beside the list `listed` of what they are; a failure leaves none of
    /// them.
    fn keep(
        &self,
        id: &SpaceId,
        listed: &[SessionFile],
        copies: &[(PathBuf, PathBuf)],
    ) -> Result<()> {
        self.remove_one(id)?;
        if copies.is_empty() {
            return Ok(());
        }
        create_marked_dir(&self.root, self.owner.as_str())
            .with_context(|| format!("create {}", self.root.display()))?;
        ensure!(
            is_marked_dir(&self.root, self.owner.as_str()),
            "{} is not a directory Shoal created for session files; move it away",
            self.root.display()
        );
        let directory = self.root.join(id);
        std::fs::create_dir(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let written = copies
            .iter()
            .try_for_each(|(source, destination)| {
                std::fs::copy(source, destination)
                    .map(drop)
                    .with_context(|| format!("copy {}", source.display()))
            })
            .and_then(|()| {
                let list = directory.join(LIST);
                std::fs::write(&list, serde_json::to_vec(listed)?)
                    .with_context(|| format!("write {}", list.display()))
            });
        if written.is_err() {
            remove_dir(&directory)?;
        }
        written
    }

    /// The list of the files kept for the workspace `id`.
    fn listed(&self, id: &SpaceId) -> Result<Vec<SessionFile>> {
        if !is_marked_dir(&self.root, self.owner.as_str()) {
            return Ok(Vec::new());
        }
        let list = self.root.join(id).join(LIST);
        match std::fs::read(&list) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| format!("read {}", list.display()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error).with_context(|| format!("read {}", list.display())),
        }
    }

    /// Move the files kept for the workspace `from` to the workspace `to`.
    fn hand_over(&self, from: &SpaceId, to: &SpaceId) -> Result<()> {
        ensure!(
            is_marked_dir(&self.root, self.owner.as_str()),
            "{} is not a directory Shoal created for session files",
            self.root.display()
        );
        let (from, to) = (self.root.join(from), self.root.join(to));
        std::fs::rename(&from, &to)
            .with_context(|| format!("move {} to {}", from.display(), to.display()))
    }

    fn remove_one(&self, id: &SpaceId) -> Result<()> {
        if !is_marked_dir(&self.root, self.owner.as_str()) {
            return Ok(());
        }
        remove_dir(&self.root.join(id))
    }

    /// Delete the files kept for workspaces other than `owners`.
    fn remove_unowned(&self, owners: &HashSet<String>) -> Result<()> {
        if !is_marked_dir(&self.root, self.owner.as_str()) {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && !owners.contains(entry.file_name().to_string_lossy().as_ref())
            {
                remove_dir(&entry.path())?;
            }
        }
        Ok(())
    }

    fn remove(&self) -> Result<()> {
        if !is_marked_dir(&self.root, self.owner.as_str()) {
            return Ok(());
        }
        remove_dir(&self.root)
    }
}

fn remove_dir(directory: &Path) -> Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

/// The IDs of the workspaces and removal records that keep their session
/// files.
fn session_owners(db: &Connection) -> Result<HashSet<String>> {
    db.prepare("SELECT id FROM workspaces UNION SELECT workspace_id FROM removed_workspaces")?
        .query_map([], |row| row.get(0))?
        .map(|id| Ok(id?))
        .collect()
}
