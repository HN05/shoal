//! The agents' session files an import carried, kept with the removal
//! record it made until the restore that opens the space puts them in place
//! with the space's own settings. A repository keeps them in its
//! directory's `.sessions`, and `root_dir` those of the chats without one,
//! a directory per space named by its ID, holding each file under its place
//! in the list beside them. Files a restore hands over but its caller does
//! not put in place stay listed there for the space's next removal record.
use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::workspace::Manager;
use crate::{
    daemon::workspace_files::HomeKey,
    model::{RemovedSpace, Repository, Space},
    shoal_dirs::{Contents, ShoalDir, remove_dir},
    transfer::{CarriedSessionFile, SessionFile, SessionSource},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Keep `files` for `removed`, imported here, replacing what an earlier
    /// import of it kept, and list them in its record. The caller holds the
    /// checkpoint gate, and for a space of a repository its Git gate, so the
    /// sweep cannot take them before the record exists.
    pub(super) async fn keep_imported_sessions(
        &self,
        mut removed: RemovedSpace,
        files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let listed: Vec<_> = files.iter().map(|carried| carried.file.clone()).collect();
        removed.session_files = listed.clone();
        let home = self.removed_home(&removed).await?;
        let layout = self.sessions_layout(&home);
        let copies: Vec<_> = files
            .iter()
            .zip(layout.kept(&removed.space_id, &listed))
            .map(|(carried, kept)| (carried.contents.clone(), kept.contents))
            .collect();
        let space_id = removed.space_id.clone();
        tokio::task::spawn_blocking(move || layout.keep(&space_id, &listed, &copies))
            .await
            .context("keep the agents' session files")??;
        Ok(removed)
    }

    /// Forget the session files kept for the removed space `removed`.
    pub(super) async fn discard_imported_sessions(&self, removed: &RemovedSpace) -> Result<()> {
        let home = self.removed_home(removed).await?;
        self.discard_sessions_in(&home, &removed.space_id).await
    }

    /// Forget the session files `home` keeps for the space `id`.
    async fn discard_sessions_in(&self, home: &HomeKey, id: &SpaceId) -> Result<()> {
        let layout = self.sessions_layout(home);
        let id = id.clone();
        tokio::task::spawn_blocking(move || layout.remove_one(&id))
            .await
            .context("delete the kept session files")?
    }

    /// Forget the session files the restore that opened `space` handed
    /// over, once the caller has put them in place.
    pub async fn drop_restored_sessions(&self, space: &SpaceSelector) -> Result<()> {
        let space = self.space(space).await?;
        let home = self.present_home(&space).await?;
        self.discard_sessions_in(&home, &space.id).await
    }

    /// The session files kept for `removed`, which the space restoring it
    /// under its ID owns where they are, for the caller to put in place.
    pub(super) async fn imported_sessions(
        &self,
        removed: &RemovedSpace,
    ) -> Result<Vec<CarriedSessionFile>> {
        let session_files = &removed.session_files;
        if session_files.is_empty() {
            return Ok(Vec::new());
        }
        let home = self.removed_home(removed).await?;
        let layout = self.sessions_layout(&home);
        Ok(layout.kept(&removed.space_id, session_files))
    }

    /// The session files kept for `space` that its restore's caller did not
    /// put in place, for its removal record. An unreadable list is not worth
    /// keeping the space for.
    pub(crate) async fn unplaced_sessions(&self, space: &Space) -> Vec<SessionFile> {
        let Ok(home) = self.present_home(space).await else {
            return Vec::new();
        };
        let layout = self.sessions_layout(&home);
        let id = space.id.clone();
        match tokio::task::spawn_blocking(move || layout.listed(&id)).await {
            Ok(Ok(listed)) => listed,
            _ => Vec::new(),
        }
    }

    /// Delete the session files `home` keeps for spaces that neither exist
    /// nor have a removal record left. The caller holds the checkpoint gate.
    pub(super) async fn expire_imported_sessions(&self, home: &HomeKey) -> Result<()> {
        let layout = self.sessions_layout(home);
        let owners = self.store.run(|db| session_owners(db)).await?;
        tokio::task::spawn_blocking(move || layout.remove_unowned(&owners))
            .await
            .context("delete expired session files")?
    }

    /// The kept session files of `removed` as sources of its export.
    pub(crate) async fn kept_session_sources(&self, removed: &RemovedSpace) -> Vec<SessionSource> {
        if removed.session_files.is_empty() {
            return Vec::new();
        }
        let Ok(home) = self.removed_home(removed).await else {
            return Vec::new();
        };
        let layout = self.sessions_layout(&home);
        layout
            .kept(&removed.space_id, &removed.session_files)
            .into_iter()
            .map(|kept| SessionSource {
                agent: kept.file.agent,
                session: kept.file.session,
                path: kept.file.path,
                source: kept.contents,
            })
            .collect()
    }

    /// The home that keeps the session files of the removed space
    /// `removed`.
    async fn removed_home(&self, removed: &RemovedSpace) -> Result<HomeKey> {
        let root_dir = removed.chat().and_then(|chat| chat.root_dir.as_deref());
        self.home_of(removed.repository_id.as_ref(), root_dir).await
    }

    /// The home that keeps the session files of `space`.
    async fn present_home(&self, space: &Space) -> Result<HomeKey> {
        (self.home_of(space.repository_id.as_ref(), space.workdir.root_dir())).await
    }

    /// The kept session files of `home`.
    fn sessions_layout(&self, home: &HomeKey) -> Layout {
        match home {
            HomeKey::Repository(repo) => Layout::of(repo),
            HomeKey::Root(root_dir) => self.outside_sessions_layout(root_dir),
        }
    }

    /// The kept session files of the chats without a repository in
    /// `root_dir`, marked as this daemon's.
    pub(super) fn outside_sessions_layout(&self, root_dir: &Path) -> Layout {
        Layout {
            dir: ShoalDir::outside_repositories(
                root_dir,
                &self.root_dir_owner(),
                Contents::ImportedSessions,
            ),
        }
    }
}

/// Delete every kept session file of `repo` with their directory, when the
/// repository goes; a directory Shoal did not create is the user's and
/// stays.
pub(super) async fn remove_imported_sessions(repo: &Repository) -> Result<()> {
    let layout = Layout::of(repo);
    tokio::task::spawn_blocking(move || layout.remove())
        .await
        .context("delete the kept session files")?
}

/// The file beside a workspace's kept session files that lists them.
const LIST: &str = "files.json";

/// The directory of one repository's kept session files, or of those of
/// the chats without a repository.
pub(super) struct Layout {
    dir: ShoalDir,
}

impl Layout {
    fn root(&self) -> &Path {
        self.dir.root()
    }

    /// The repository's kept session files.
    fn of(repo: &Repository) -> Self {
        Self {
            dir: ShoalDir::of(repo, Contents::ImportedSessions),
        }
    }

    /// The directory, when this Shoal created it for its owner.
    pub(super) fn own_root(&self) -> Option<&Path> {
        self.dir.is_own().then(|| self.root())
    }

    /// The directory of the session files kept for the space `id`, when
    /// this Shoal keeps one there.
    pub(super) fn existing(&self, id: &SpaceId) -> Option<PathBuf> {
        let directory = self.root().join(id);
        (self.dir.is_own() && directory.is_dir()).then_some(directory)
    }

    /// The new, empty directory of the session files kept for the space
    /// `id`, creating the marked directory when missing.
    pub(super) fn prepare(&self, id: &SpaceId) -> Result<PathBuf> {
        self.dir.create()?;
        let directory = self.root().join(id);
        std::fs::create_dir(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        Ok(directory)
    }

    /// The session files `files` kept for the workspace `id`, each named by
    /// its place among them.
    fn kept(&self, id: &SpaceId, files: &[SessionFile]) -> Vec<CarriedSessionFile> {
        let directory = self.root().join(id);
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
        let directory = self.prepare(id)?;
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
        if !self.dir.is_own() {
            return Ok(Vec::new());
        }
        let list = self.root().join(id).join(LIST);
        match std::fs::read(&list) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| format!("read {}", list.display()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error).with_context(|| format!("read {}", list.display())),
        }
    }

    fn remove_one(&self, id: &SpaceId) -> Result<()> {
        if !self.dir.is_own() {
            return Ok(());
        }
        remove_dir(&self.root().join(id))
    }

    /// Delete the files kept for workspaces other than `owners`.
    fn remove_unowned(&self, owners: &HashSet<String>) -> Result<()> {
        if !self.dir.is_own() {
            return Ok(());
        }
        for entry in std::fs::read_dir(self.root())? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && !owners.contains(entry.file_name().to_string_lossy().as_ref())
            {
                remove_dir(&entry.path())?;
            }
        }
        Ok(())
    }

    pub(super) fn remove(&self) -> Result<()> {
        self.dir.remove()
    }
}

/// The IDs of the spaces and removal records that keep their session
/// files.
fn session_owners(db: &Connection) -> Result<HashSet<String>> {
    db.prepare("SELECT id FROM spaces UNION SELECT space_id FROM removed_spaces")?
        .query_map([], |row| row.get(0))?
        .map(|id| Ok(id?))
        .collect()
}
