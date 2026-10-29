//! How long attached files stay: a workspace's files go with it, except its
//! results, which stay as long as its removal record, and every file goes
//! with its repository.
use std::path::Path;

use anyhow::{Result, ensure};

use super::{Owner, layout::Layout, records};
use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
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

    /// Delete the results of removed workspaces whose removal record has
    /// expired, or that never had one. A failure keeps what it concerns for
    /// the next pass and does not skip the rest.
    pub async fn expire_results(&self) -> Result<()> {
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
        for workspace_id in expired {
            self.delete_results_of(&repo, workspace_id).await?;
        }
        Ok(())
    }

    /// The removed workspaces whose results are kept, those that handed them
    /// back first oldest first.
    pub(crate) async fn kept_results(&self) -> Result<Vec<Owner>> {
        self.store.run(|db| records::kept(db)).await
    }

    /// Delete the results that the removed workspace `owner` handed back
    /// when they are where `holds` says, as on a filesystem low on space,
    /// and return whether it did. The place is checked with the files gate
    /// held, so a move of the repository's directory cannot come between.
    pub(crate) async fn delete_results_in(
        &self,
        owner: &Owner,
        holds: impl Fn(&Path) -> bool,
    ) -> Result<bool> {
        let (repo, _files) = self.lock_repository_files(&owner.repository_id).await?;
        let there = Layout::of(&repo).is_some_and(|layout| holds(&layout.results(&owner.id)));
        if there {
            self.delete_results_of(&repo, owner.id.clone()).await?;
        }
        Ok(there)
    }

    /// Delete the results of the workspace `workspace_id` of `repo`, and then
    /// their records. Callers hold the gate of the repository's files.
    async fn delete_results_of(&self, repo: &Repository, workspace_id: String) -> Result<()> {
        if let Some(layout) = Layout::of(repo) {
            let id = workspace_id.clone();
            tokio::task::spawn_blocking(move || layout.remove_results(&id)).await??;
        }
        self.store
            .run(move |db| records::forget_workspace(db, &workspace_id))
            .await
    }
}
