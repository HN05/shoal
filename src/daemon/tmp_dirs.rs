//! Deleting the temporary directories of workspaces and chats once they are
//! gone.
use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{
    daemon::{log, workspace::Manager},
    model::Chat,
    tmp_dirs::Layout,
};

impl Manager {
    /// Delete the temporary directory of the workspace or chat `id` of the
    /// repository `repository_id` once its record is gone.
    pub(crate) async fn delete_tmp_dir(&self, repository_id: &str, id: &str) -> Result<()> {
        let repo = self.repository(repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        remove(layout, vec![id.to_owned()]).await
    }

    /// Delete the temporary directory of `chat` once its record is gone.
    pub(crate) async fn delete_chat_tmp_dir(&self, chat: &Chat) -> Result<()> {
        let repo = self.chat_repository(chat).await?;
        let Some(layout) = self.chat_tmp_layout(repo.as_ref(), chat) else {
            return Ok(());
        };
        remove(layout, vec![chat.id.clone()]).await
    }

    /// Delete every temporary directory that belongs to no workspace or chat
    /// of its repository, or to no chat without one, such as one a process
    /// created while its workspace was removed. A failure in one repository
    /// does not skip the others.
    pub(crate) async fn sweep_tmp_dirs(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if let Err(error) = self
                .sweep_layout_tmp_dirs(Layout::of(&repo), Some(&repo.id))
                .await
            {
                failed.push(format!("{}: {error:#}", crate::repository::name(&repo)));
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
        repository_id: Option<&str>,
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
        if !orphans.is_empty() {
            log!(
                "deleting orphaned temporary directories {}",
                orphans.join(", ")
            );
        }
        remove(layout, orphans).await
    }

    /// The IDs of the repository's workspaces and chats, or of the chats
    /// without a repository.
    async fn recorded_ids(&self, repository_id: Option<&str>) -> Result<BTreeSet<String>> {
        let repository_id = repository_id.map(str::to_owned);
        self.store
            .run(move |db| recorded_ids(db, repository_id.as_deref()))
            .await
    }
}

/// The IDs whose temporary directories `layout` holds.
async fn list(layout: Layout) -> Result<Vec<String>> {
    tokio::task::spawn_blocking(move || layout.list()).await?
}

/// Delete the temporary directories of `ids` in `layout`.
async fn remove(layout: Layout, ids: Vec<String>) -> Result<()> {
    tokio::task::spawn_blocking(move || ids.iter().try_for_each(|id| layout.remove_one(id))).await?
}

/// The IDs of the repository's workspaces and chats, or of the chats
/// without a repository.
fn recorded_ids(db: &mut Connection, repository_id: Option<&str>) -> Result<BTreeSet<String>> {
    Ok(db
        .prepare(
            "SELECT id FROM workspaces WHERE repository_id IS ?1
             UNION SELECT id FROM chats WHERE repository_id IS ?1",
        )?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
