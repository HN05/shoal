//! Where the temporary directories of spaces are, and deleting them once
//! their spaces are gone.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{
    daemon::{log, manager::Manager},
    model::{Repository, Space},
    tmp_dirs::Layout,
};

impl Manager {
    /// The layout that keeps the temporary directories of the spaces of
    /// `repository`, or for a chat without one of the `root_dir` it started
    /// in; none for such a chat that recorded none.
    pub(crate) fn tmp_layout(
        &self,
        repository: Option<&Repository>,
        root_dir: Option<&Path>,
    ) -> Option<Layout> {
        match repository {
            Some(repository) => Some(Layout::of(repository)),
            None => root_dir.map(|root| Layout::outside_repositories(root, &self.root_dir_owner())),
        }
    }

    /// The layout that keeps the temporary directory of `space`.
    pub(crate) async fn space_tmp_layout(&self, space: &Space) -> Result<Option<Layout>> {
        let repository = match space.repository_id.as_ref() {
            Some(id) => Some(self.repositories.repository(id).await?),
            None => None,
        };
        Ok(self.tmp_layout(repository.as_ref(), space.workdir.root_dir()))
    }

    /// Delete the temporary directory of `space` once its record is gone.
    pub(crate) async fn delete_tmp_dir(&self, space: &Space) -> Result<()> {
        let Some(layout) = self.space_tmp_layout(space).await? else {
            return Ok(());
        };
        remove(layout, vec![space.id.to_owned()])
            .await?
            .into_result()
    }

    /// Delete every temporary directory that belongs to no workspace or chat
    /// of its repository, or to no chat without one, such as one a process
    /// created while its workspace was removed. A directory that cannot be
    /// deleted keeps neither the others nor the other repositories'.
    pub(crate) async fn sweep_tmp_dirs(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories.list().await? {
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

    /// Delete the temporary directories in `layout` of no workspace or chat
    /// of `repository_id`, or of no chat without a repository.
    async fn sweep_layout_tmp_dirs(
        &self,
        layout: Option<Layout>,
        repository_id: Option<&RepositoryId>,
    ) -> Result<()> {
        let Some(layout) = layout else {
            return Ok(());
        };
        // Listed before the records are read, since a directory is only
        // created once its record exists.
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

    /// The IDs of the repository's workspaces and chats, or of the chats
    /// without a repository.
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

/// The spaces with a temporary directory in `layout`, which names each by
/// its space's ID.
async fn list(layout: Layout) -> Result<Vec<SpaceId>> {
    let names = tokio::task::spawn_blocking(move || layout.list()).await??;
    Ok(names.into_iter().map(SpaceId::from).collect())
}

/// What deleting temporary directories did.
#[derive(Debug, Default)]
struct Removed {
    /// The spaces whose temporary directories it deleted.
    deleted: Vec<SpaceId>,
    /// Why each of the others could not be deleted.
    failed: Vec<String>,
}

impl Removed {
    /// Fails naming each directory that could not be deleted.
    fn into_result(self) -> Result<()> {
        ensure!(self.failed.is_empty(), "{}", self.failed.join("; "));
        Ok(())
    }
}

/// Delete the temporary directories of `ids` in `layout`, each whether or
/// not the others could be.
async fn remove(layout: Layout, ids: Vec<SpaceId>) -> Result<Removed> {
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

/// The IDs of the repository's workspaces and chats, or of the chats
/// without a repository.
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
