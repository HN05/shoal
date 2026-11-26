//! The spaces' temporary directories, deleted once their spaces are gone.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{
    log,
    model::{Repository, Space},
};
use shoal_core::tmp_dirs::Layout;

impl Spaces {
    /// For a chat without a repository, the layout of the `root_dir` it
    /// started in; none for such a chat that recorded none.
    pub fn tmp_layout(
        &self,
        repository: Option<&Repository>,
        root_dir: Option<&Path>,
    ) -> Option<Layout> {
        match repository {
            Some(repository) => Some(Layout::of(repository)),
            None => root_dir.map(|root| Layout::outside_repositories(root, &self.root_dir_owner())),
        }
    }

    pub async fn space_tmp_layout(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<Option<Layout>> {
        let repository = match space.repository_id.as_ref() {
            Some(id) => Some(
                daemon
                    .repository(crate::repository::Selector::from(id))
                    .await?,
            ),
            None => None,
        };
        Ok(self.tmp_layout(repository.as_ref(), space.workdir.root_dir()))
    }

    /// Call once the space's record is gone.
    pub async fn delete_tmp_dir(&self, daemon: &dyn Daemon, space: &Space) -> Result<()> {
        let Some(layout) = self.space_tmp_layout(daemon, space).await? else {
            return Ok(());
        };
        remove(layout, vec![space.id.to_owned()])
            .await?
            .into_result()
    }

    /// Deletes every temporary directory that belongs to no space, such as
    /// one a process created while its workspace was removed. A directory
    /// that cannot be deleted keeps neither the others nor the other
    /// repositories' from going.
    pub async fn sweep_tmp_dirs(&self, daemon: &dyn Daemon) -> Result<()> {
        let mut failed = Vec::new();
        for repo in daemon.repositories().await? {
            if let Err(error) = self
                .sweep_layout_tmp_dirs(Some(Layout::of(&repo)), Some(&repo.id))
                .await
            {
                failed.push(format!("{}: {error:#}", repo.display_name()));
            }
        }
        let outside = self.outside_tmp_layout();
        if let Err(error) = self.sweep_layout_tmp_dirs(outside, None).await {
            failed.push(format!("chats without a repository: {error:#}"));
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn sweep_layout_tmp_dirs(
        &self,
        layout: Option<Layout>,
        repository_id: Option<&RepositoryId>,
    ) -> Result<()> {
        let Some(layout) = layout else {
            return Ok(());
        };
        // List before reading the records: a directory is only created once
        // its record exists.
        let listed = list(layout.clone()).await?;
        if listed.is_empty() {
            return Ok(());
        }
        let recorded = self.recorded_ids(repository_id).await?;
        let orphans: Vec<_> = listed
            .into_iter()
            .filter(|id| !recorded.contains(id))
            .collect();
        let removed = remove(layout, orphans).await?;
        if !removed.deleted.is_empty() {
            log!(
                "deleted orphaned temporary directories {}",
                removed.deleted.join(", ")
            );
        }
        removed.into_result()
    }

    /// The chats without a repository when `repository_id` is `None`.
    async fn recorded_ids(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> Result<BTreeSet<SpaceId>> {
        let repository_id = repository_id.cloned();
        self.store
            .run(move |db| recorded_ids(db, repository_id.as_ref()))
            .await
    }
}

/// The layout names each directory by its space's ID.
async fn list(layout: Layout) -> Result<Vec<SpaceId>> {
    let names = tokio::task::spawn_blocking(move || layout.list()).await??;
    Ok(names.into_iter().map(SpaceId::from).collect())
}

#[derive(Debug, Default)]
pub struct Removed {
    pub deleted: Vec<SpaceId>,
    /// Why each of the others could not be deleted.
    pub failed: Vec<String>,
}

impl Removed {
    /// Fails naming each directory that could not be deleted.
    pub fn into_result(self) -> Result<()> {
        ensure!(self.failed.is_empty(), "{}", self.failed.join("; "));
        Ok(())
    }
}

/// Deletes each whether or not the others could be.
pub async fn remove(layout: Layout, ids: Vec<SpaceId>) -> Result<Removed> {
    let removed = tokio::task::spawn_blocking(move || {
        let mut removed = Removed::default();
        for id in ids {
            match layout.remove_one(&id) {
                Ok(()) => removed.deleted.push(id),
                Err(error) => removed.failed.push(format!("{error:#}")),
            }
        }
        removed
    });
    Ok(removed.await?)
}

/// The chats without a repository when `repository_id` is `None`.
fn recorded_ids(
    db: &mut Connection,
    repository_id: Option<&RepositoryId>,
) -> Result<BTreeSet<SpaceId>> {
    Ok(db
        .prepare("SELECT id FROM spaces WHERE repository_id IS ?1")?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
