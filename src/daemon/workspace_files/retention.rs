//! How long attached files stay: a workspace's files go with it, except its
//! results, which stay as long as its removal record; a chat's stay for the
//! same retention after it ends; and every file goes with its repository.
use std::path::Path;

use anyhow::{Result, ensure};

use super::{Owner, layout::Layout, records};
use crate::{
    daemon::{log, workspace::Manager},
    model::{Repository, Workspace},
    time::unix_seconds,
};

impl Manager {
    /// Delete the files attached to `workspace` once it is removed; its
    /// results stay until [`Manager::expire_results`]. The removal's own
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

    /// When the files of the chat in repository `repository_id` expire if
    /// it ends now: after the repository's `cleanup.restore.retention_days`,
    /// as a removed workspace's results do, or the global setting's when the
    /// repository's cannot be read.
    pub(crate) async fn chat_files_expiry(&self, repository_id: &str) -> i64 {
        let settings = match self.repository_settings(repository_id).await {
            Ok(settings) => Ok(settings),
            Err(error) => {
                log!("chat files of {repository_id} kept by the global retention: {error:#}");
                self.config().resolve(&Default::default())
            }
        };
        let retention =
            settings.map_or(0, |settings| settings.cleanup.restore.retention().as_secs());
        i64::try_from(unix_seconds() + retention).unwrap_or(i64::MAX)
    }

    /// Delete the files kept for removed workspaces and ended chats whose
    /// record has expired, or that never had one. A failure keeps what it
    /// concerns for the next pass and does not skip the rest.
    pub async fn expire_results(&self) -> Result<()> {
        self.store.run(records::forget_expired_chats).await?;
        let mut failed = Vec::new();
        for repo in self.repositories().await? {
            if let Err(error) = self.expire_repository_results(&repo).await {
                failed.push(format!("{}: {error:#}", repo.path.display()));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_repository_results(&self, listed: &Repository) -> Result<()> {
        let (repo, _files) = self.lock_repository_files(&listed.id).await?;
        let id = repo.id.clone();
        let expired = self.store.run(move |db| records::expired(db, &id)).await?;
        for owner_id in expired {
            self.delete_kept_files(&repo, owner_id).await?;
        }
        Ok(())
    }

    /// The removed workspaces and ended chats whose files are kept, those
    /// that attached them first oldest first.
    pub(crate) async fn kept_results(&self) -> Result<Vec<Owner>> {
        self.store.run(|db| records::kept(db)).await
    }

    /// Delete the files kept for the removed workspace or ended chat `owner`
    /// when they are where `holds` says, as on a filesystem low on space,
    /// and return whether it did. The place is checked with the files gate
    /// held, so a move of the repository's directory cannot come between.
    pub(crate) async fn delete_results_in(
        &self,
        owner: &Owner,
        holds: impl Fn(&Path) -> bool,
    ) -> Result<bool> {
        let (repo, _files) = self.lock_repository_files(&owner.repository_id).await?;
        let there = Layout::of(&repo)
            .is_some_and(|layout| layout.kept(&owner.id).iter().any(|path| holds(path)));
        if there {
            self.delete_kept_files(&repo, owner.id.clone()).await?;
        }
        Ok(there)
    }

    /// Delete the files kept for the removed workspace or ended chat
    /// `owner_id` of `repo`, and then their records. Callers hold the gate of
    /// the repository's files.
    async fn delete_kept_files(&self, repo: &Repository, owner_id: String) -> Result<()> {
        if let Some(layout) = Layout::of(repo) {
            let id = owner_id.clone();
            tokio::task::spawn_blocking(move || layout.remove_kept(&id)).await??;
        }
        self.store
            .run(move |db| records::forget_owner(db, &owner_id))
            .await
    }
}
