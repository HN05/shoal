//! How long attached files stay: a workspace's files go with it, except its
//! results, which stay as long as its removal record; a chat's stay for
//! `cleanup.restore.retention_days` after it ends; and every file goes with
//! its repository.
use std::path::Path;

use anyhow::{Result, ensure};

use super::{Owner, layout::Layout, records};
use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
};

impl Manager {
    /// Delete the files attached to `workspace` once it is removed; its
    /// results stay until [`Manager::expire_kept_files`]. The removal's own
    /// transaction already forgot their records.
    pub(crate) async fn delete_workspace_files(&self, workspace: &Workspace) -> Result<()> {
        let (repo, _files) = self.lock_repository_files(&workspace.repository_id).await?;
        let Some(layout) = Layout::of(&repo) else {
            return Ok(());
        };
        let id = workspace.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_workspace(&id)).await?
    }

    /// Delete the directory of attached files with the repository, once its
    /// workspaces are gone.
    pub(crate) async fn delete_repository_files(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        // Records outlive copies a failed deletion leaves, so they stay listed.
        if let Some(layout) = Layout::of(repo) {
            tokio::task::spawn_blocking(move || layout.remove()).await??;
        }
        let id = repo.id.clone();
        self.store
            .run(move |db| records::forget_repository(db, &id))
            .await
    }

    /// Delete the results of removed workspaces whose removal record has
    /// expired, or that never had one, and the files of chats that ended
    /// longer ago than their repository keeps them. A failure keeps what it
    /// concerns for the next pass and does not skip the rest.
    pub async fn expire_kept_files(&self) -> Result<()> {
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if let Err(error) = self.expire_repository_files(&repo).await {
                failed.push(format!("{}: {error:#}", repo.path.display()));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_repository_files(&self, listed: &Repository) -> Result<()> {
        self.stamp_ended_chat_files(&listed.id).await?;
        let (repo, _files) = self.lock_repository_files(&listed.id).await?;
        let id = repo.id.clone();
        let expired = self.store.run(move |db| records::expired(db, &id)).await?;
        for owner in expired {
            self.delete_kept_of(&repo, owner).await?;
        }
        Ok(())
    }

    /// Give the files of repository `repository_id`'s ended chats the time
    /// they go, `cleanup.restore.retention_days` from now, unless they have
    /// one.
    pub(crate) async fn stamp_ended_chat_files(&self, repository_id: &str) -> Result<()> {
        let retention = self
            .repository_settings(repository_id)
            .await?
            .cleanup
            .restore
            .retention();
        let expires_at = i64::try_from(crate::time::unix_seconds() + retention.as_secs())?;
        let id = repository_id.to_owned();
        self.store
            .run(move |db| records::stamp_ended_chats(db, &id, expires_at))
            .await
    }

    /// The removed workspaces and ended chats whose files are kept, those
    /// that kept them first oldest first.
    pub(crate) async fn kept_file_owners(&self) -> Result<Vec<Owner>> {
        self.store.run(|db| records::kept(db)).await
    }

    /// Delete the files that the removed workspace or ended chat `owner`
    /// kept when they are where `holds` says, as on a filesystem low on
    /// space, and return whether it did. The place is checked with the files
    /// gate held, so a move of the repository's directory cannot come
    /// between.
    pub(crate) async fn delete_kept_in(
        &self,
        owner: &Owner,
        holds: impl Fn(&Path) -> bool,
    ) -> Result<bool> {
        let (repo, _files) = self.lock_repository_files(&owner.repository_id).await?;
        let there =
            Layout::of(&repo).is_some_and(|layout| holds(&layout.kept(owner.kind, &owner.id)));
        if there {
            self.delete_kept_of(&repo, owner.clone()).await?;
        }
        Ok(there)
    }

    /// Delete the files that the removed workspace or ended chat `owner` of
    /// `repo` kept, and then their records. Callers hold the gate of the
    /// repository's files.
    async fn delete_kept_of(&self, repo: &Repository, owner: Owner) -> Result<()> {
        if let Some(layout) = Layout::of(repo) {
            let (kind, id) = (owner.kind, owner.id.clone());
            tokio::task::spawn_blocking(move || layout.remove_kept(kind, &id)).await??;
        }
        self.store
            .run(move |db| records::forget_owner(db, &owner.id))
            .await
    }
}
