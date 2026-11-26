//! The agents' session files an import carried, kept with the removal record
//! until the restore that opens the space puts them in place. A directory
//! per space, named by its ID, holds each file under its place in the list
//! beside them. See `docs/design/moves.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_protocol::transcripts::CarriedSessionFile;
use shoal_protocol::transcripts::SessionFile;
use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::{
    model::{RemovedSpace, Repository, Space},
    shoal_dirs::{Contents, ShoalDir, remove_dir},
    transfer::SessionSource,
    workspace_files::HomeKey,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Replaces what an earlier import of the space kept. The caller holds
    /// the checkpoint gate, and for a space of a repository its Git gate, so
    /// the sweep cannot take the files before the record exists.
    pub async fn keep_imported_sessions(
        &self,
        daemon: &dyn Daemon,
        mut removed: RemovedSpace,
        files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let listed: Vec<_> = files.iter().map(|carried| carried.file.clone()).collect();
        removed.session_files = listed.clone();
        let home = self.removed_home(daemon, &removed).await?;
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

    pub async fn discard_imported_sessions(
        &self,
        daemon: &dyn Daemon,
        removed: &RemovedSpace,
    ) -> Result<()> {
        let home = self.removed_home(daemon, removed).await?;
        self.discard_sessions_in(&home, &removed.space_id).await
    }

    async fn discard_sessions_in(&self, home: &HomeKey, id: &SpaceId) -> Result<()> {
        let layout = self.sessions_layout(home);
        let id = id.clone();
        tokio::task::spawn_blocking(move || layout.remove_one(&id))
            .await
            .context("delete the kept session files")?
    }

    /// Call once the caller has put the files in place.
    pub async fn drop_restored_sessions(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
    ) -> Result<()> {
        let space = self.space(space).await?;
        let home = self.present_home(daemon, &space).await?;
        self.discard_sessions_in(&home, &space.id).await
    }

    /// The space that restores `removed` under its ID owns the files where
    /// they are, for the caller to put in place.
    pub async fn imported_sessions(
        &self,
        daemon: &dyn Daemon,
        removed: &RemovedSpace,
    ) -> Result<Vec<CarriedSessionFile>> {
        let session_files = &removed.session_files;
        if session_files.is_empty() {
            return Ok(Vec::new());
        }
        let home = self.removed_home(daemon, removed).await?;
        let layout = self.sessions_layout(&home);
        Ok(layout.kept(&removed.space_id, session_files))
    }

    /// The files a restore's caller did not put in place, for the space's
    /// next removal record. An unreadable list is not worth keeping the
    /// space for.
    pub async fn unplaced_sessions(&self, daemon: &dyn Daemon, space: &Space) -> Vec<SessionFile> {
        let Ok(home) = self.present_home(daemon, space).await else {
            return Vec::new();
        };
        let layout = self.sessions_layout(&home);
        let id = space.id.clone();
        match tokio::task::spawn_blocking(move || layout.listed(&id)).await {
            Ok(Ok(listed)) => listed,
            _ => Vec::new(),
        }
    }

    /// Deletes the files of spaces that neither exist nor have a removal
    /// record left. The caller holds the checkpoint gate.
    pub async fn expire_imported_sessions(&self, home: &HomeKey) -> Result<()> {
        let layout = self.sessions_layout(home);
        let owners = self.store.run(|db| session_owners(db)).await?;
        tokio::task::spawn_blocking(move || layout.remove_unowned(&owners))
            .await
            .context("delete expired session files")?
    }

    pub async fn kept_session_sources(
        &self,
        daemon: &dyn Daemon,
        removed: &RemovedSpace,
    ) -> Vec<SessionSource> {
        if removed.session_files.is_empty() {
            return Vec::new();
        }
        let Ok(home) = self.removed_home(daemon, removed).await else {
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

    async fn removed_home(&self, daemon: &dyn Daemon, removed: &RemovedSpace) -> Result<HomeKey> {
        let root_dir = removed.chat().and_then(|chat| chat.root_dir.as_deref());
        self.home_of(daemon, removed.repository_id.as_ref(), root_dir)
            .await
    }

    async fn present_home(&self, daemon: &dyn Daemon, space: &Space) -> Result<HomeKey> {
        (self.home_of(
            daemon,
            space.repository_id.as_ref(),
            space.workdir.root_dir(),
        ))
        .await
    }

    fn sessions_layout(&self, home: &HomeKey) -> Layout {
        match home {
            HomeKey::Repository(repo) => Layout::of(repo),
            HomeKey::Root(root_dir) => self.outside_sessions_layout(root_dir),
        }
    }

    /// Marked as this daemon's.
    pub fn outside_sessions_layout(&self, root_dir: &Path) -> Layout {
        Layout {
            dir: ShoalDir::outside_repositories(
                root_dir,
                &self.root_dir_owner(),
                Contents::ImportedSessions,
            ),
        }
    }
}

/// Runs when the repository goes. A directory Shoal did not create is the
/// user's and stays.
pub async fn remove_imported_sessions(repo: &Repository) -> Result<()> {
    let layout = Layout::of(repo);
    tokio::task::spawn_blocking(move || layout.remove())
        .await
        .context("delete the kept session files")?
}

const LIST: &str = "files.json";

pub struct Layout {
    dir: ShoalDir,
}

impl Layout {
    fn root(&self) -> &Path {
        self.dir.root()
    }

    fn of(repo: &Repository) -> Self {
        Self {
            dir: ShoalDir::of(repo, Contents::ImportedSessions),
        }
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

    /// Each file is named by its place in `files`.
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

    /// The space's directory starts empty, and is left out when there are no
    /// copies. A failure leaves none of them.
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

    pub fn remove(&self) -> Result<()> {
        self.dir.remove()
    }
}

fn session_owners(db: &Connection) -> Result<HashSet<String>> {
    db.prepare("SELECT id FROM spaces UNION SELECT space_id FROM removed_spaces")?
        .query_map([], |row| row.get(0))?
        .map(|id| Ok(id?))
        .collect()
}
