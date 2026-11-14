//! How long attached files stay: a removed space's stay for
//! `cleanup.restore.retention_days` after its removal, and every file goes
//! with its repository.
use crate::client::space_ids::SpaceId;
use std::{path::Path, time::Duration};

use anyhow::{Result, ensure};

use super::{FilesHome, HomeKey, Space, records};
use crate::{
    daemon::{log, workspace::Manager},
    model::Repository,
    time::UnixTime,
};

impl Manager {
    /// Delete the directory of attached files with the repository, once its
    /// workspaces are gone.
    pub(crate) async fn delete_repository_files(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        // Records outlive copies a failed deletion leaves, so they stay listed.
        if let Some(layout) = super::layout::Layout::of(repo) {
            tokio::task::spawn_blocking(move || layout.remove()).await??;
        }
        let id = repo.id.clone();
        self.store
            .run(move |db| records::forget_repository(db, &id))
            .await
    }

    /// When the files of `space` that outlive it expire if it is removed
    /// now: after its `cleanup.restore.retention_days`, or the global
    /// setting's when its own cannot be read.
    pub(crate) async fn removed_files_expiry(
        &self,
        space: &crate::model::PresentSpace,
    ) -> UnixTime {
        let settings = match self.present_space_settings(space).await {
            Ok(settings) => Ok(settings),
            Err(error) => {
                log!(
                    "files of {} kept by the global retention: {error:#}",
                    space.id()
                );
                self.config().resolve(&Default::default())
            }
        };
        let retention = settings.map_or(Duration::ZERO, |settings| {
            settings.cleanup.restore.retention()
        });
        UnixTime::now() + retention
    }

    /// Delete the files kept for removed workspaces and removed chats whose
    /// record has expired, or that never had one. A failure keeps what it
    /// concerns for the next pass and does not skip the rest.
    pub async fn expire_results(&self) -> Result<()> {
        let mut failed = Vec::new();
        for key in self.files_homes().await? {
            if let Err(error) = self.expire_home_results(&key).await {
                failed.push(format!("{}: {error:#}", key.describe()));
            }
        }
        self.store.run(records::forget_expired).await?;
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Delete the expired files `key` keeps for removed spaces.
    async fn expire_home_results(&self, key: &HomeKey) -> Result<()> {
        let (home, _files) = self.lock_files_home(key).await?;
        let key = key.clone();
        let expired = (self.store)
            .run(move |db| records::expired(db, &key))
            .await?;
        for space_id in expired {
            self.delete_kept_files(&home, space_id).await?;
        }
        Ok(())
    }

    /// The removed workspaces and removed chats whose files are kept, those
    /// that attached them first oldest first.
    pub(crate) async fn kept_results(&self) -> Result<Vec<Space>> {
        self.store.run(|db| records::kept(db)).await
    }

    /// Delete the files kept for the removed workspace or removed chat `space`
    /// when they, or the directories that keep them, are where `holds` says, as on a filesystem low on space,
    /// and return whether it did. The place is checked with the files gate
    /// held, so a move of the repository's directory cannot come between.
    pub(crate) async fn delete_results_in(
        &self,
        space: &Space,
        holds: impl Fn(&Path) -> bool,
    ) -> Result<bool> {
        let (home, _files) = self.lock_files_home(&self.space_home(space).await?).await?;
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
        let there = (home.layout()).is_some_and(|layout| {
            (layout.kept(&space.id).iter())
                .chain(&copies)
                .any(|path| holds(path))
        });
        if there {
            self.delete_kept_files(&home, space.id.clone()).await?;
        }
        Ok(there)
    }

    /// Delete the files kept in `home` for the removed workspace or removed
    /// chat `space_id`, and then their records. Callers hold the gate of its
    /// files.
    async fn delete_kept_files(&self, home: &FilesHome, space_id: SpaceId) -> Result<()> {
        if let Some(layout) = home.layout() {
            let (id, copies) = (space_id.clone(), self.kept_copies(&space_id).await?);
            tokio::task::spawn_blocking(move || layout.remove_kept(&id, &copies)).await??;
        }
        self.store
            .write(move |tx| records::forget_space(tx, &space_id))
            .await
    }

    /// The paths of the copies recorded for the removed space `space_id`.
    async fn kept_copies(&self, space_id: &SpaceId) -> Result<Vec<std::path::PathBuf>> {
        let id = space_id.clone();
        let files = self.store.read(move |db| records::list(db, &id)).await?;
        Ok(files.into_iter().map(|file| file.path).collect())
    }
}
