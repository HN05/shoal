//! How long attached files stay. See `docs/design/cleanup.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use std::{path::Path, time::Duration};

use anyhow::{Result, ensure};

use super::{FilesHome, HomeKey, Space, records};
use crate::{log, model::Repository, time::UnixTime};

impl Spaces {
    /// Runs once the repository's workspaces are gone.
    pub async fn delete_repository_files(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        // Records outlive the copies a failed deletion leaves, so they stay
        // listed.
        let layout = super::layout::Layout::of(repo);
        tokio::task::spawn_blocking(move || layout.remove()).await??;
        let id = repo.id.clone();
        self.store
            .run(move |db| records::forget_repository(db, &id))
            .await
    }

    /// After the space's `cleanup.restore.retention_days`, or the global
    /// setting's when its own cannot be read.
    pub async fn removed_files_expiry(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
    ) -> UnixTime {
        let settings = match daemon.space_settings(space).await {
            Ok(settings) => Ok(settings),
            Err(error) => {
                log!(
                    "files of {} kept by the global retention: {error:#}",
                    &space.id
                );
                self.config.current().resolve(&Default::default())
            }
        };
        let retention = settings.map_or(Duration::ZERO, |settings| {
            settings.cleanup.restore.retention()
        });
        UnixTime::now() + retention
    }

    /// Deletes them only when no record names a copy in them, as once `shoal
    /// files rm` deleted them all, since no expiry would delete them later.
    /// A failure is logged, since the removal already happened.
    pub async fn delete_unrecorded_files(&self, daemon: &dyn Daemon, space: &Space) {
        if let Err(error) = self.try_delete_unrecorded_files(daemon, space).await {
            log!("attached files of {} not deleted: {error:#}", space.name);
        }
    }

    async fn try_delete_unrecorded_files(&self, daemon: &dyn Daemon, space: &Space) -> Result<()> {
        let (home, _files) = self
            .lock_files_home(daemon, &self.space_home(daemon, space).await?)
            .await?;
        let layout = home.layout();
        // A restore may have given copies there to a workspace meanwhile.
        let directories = layout.kept(&space.id);
        if (self.store)
            .read(move |db| records::any_in(db, &directories))
            .await?
        {
            return Ok(());
        }
        let id = space.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_kept(&id, &[])).await?
    }

    /// Includes removed spaces that never had an expiry. A failure keeps
    /// what it concerns for the next pass and does not skip the rest.
    pub async fn expire_results(&self, daemon: &dyn Daemon) -> Result<()> {
        let mut failed = Vec::new();
        for key in self.files_homes(daemon).await? {
            if let Err(error) = self.expire_home_results(daemon, &key).await {
                failed.push(format!("{}: {error:#}", key.describe()));
            }
        }
        self.store.run(records::forget_expired).await?;
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_home_results(&self, daemon: &dyn Daemon, key: &HomeKey) -> Result<()> {
        let (home, _files) = self.lock_files_home(daemon, key).await?;
        let key = key.clone();
        let expired = (self.store)
            .run(move |db| records::expired(db, &key))
            .await?;
        for space_id in expired {
            self.delete_kept_files(&home, space_id).await?;
        }
        Ok(())
    }

    /// Those that attached them first come first.
    pub async fn kept_results(&self) -> Result<Vec<Space>> {
        self.store.run(|db| records::kept(db)).await
    }

    /// Deletes only where `holds` says the files or their directories are,
    /// as on a filesystem low on space. The place is checked with the files
    /// gate held, so a move of the repository's directory cannot come
    /// between.
    pub async fn delete_results_in(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        holds: impl Fn(&Path) -> bool,
    ) -> Result<bool> {
        let (home, _files) = self
            .lock_files_home(daemon, &self.space_home(daemon, space).await?)
            .await?;
        // A restore may have given the files to a workspace meanwhile.
        let id = space.id.clone();
        if (self
            .store
            .read(move |db| records::gone_space(db, &id))
            .await?)
            .is_none()
        {
            return Ok(false);
        }
        let copies = self.kept_copies(&space.id).await?;
        let there = (home.layout().kept(&space.id).iter())
            .chain(&copies)
            .any(|path| holds(path));
        if there {
            self.delete_kept_files(&home, space.id.clone()).await?;
        }
        Ok(there)
    }

    /// Deletes the files, then their records. Callers hold the gate of its
    /// files.
    async fn delete_kept_files(&self, home: &FilesHome, space_id: SpaceId) -> Result<()> {
        let (layout, id) = (home.layout(), space_id.clone());
        let copies = self.kept_copies(&space_id).await?;
        tokio::task::spawn_blocking(move || layout.remove_kept(&id, &copies)).await??;
        self.store
            .write(move |tx| records::forget_space(tx, &space_id))
            .await
    }

    async fn kept_copies(&self, space_id: &SpaceId) -> Result<Vec<std::path::PathBuf>> {
        let id = space_id.clone();
        let files = self.store.read(move |db| records::list(db, &id)).await?;
        Ok((files.iter())
            .filter_map(|file| file.copy.path().map(Path::to_path_buf))
            .collect())
    }
}
