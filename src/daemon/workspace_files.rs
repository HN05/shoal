//! Files attached to workspaces, such as those sent with a message to their
//! agents. Each repository keeps them beside its workspaces, in a directory
//! its agents are given as a working directory.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
    state::WorkspaceState,
};

pub(crate) mod layout;

use layout::{Kept, Layout};

/// Copies of `files`, attached to `message` for the workspace
/// `workspace_id`, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
pub(crate) async fn keep(
    layout: Layout,
    workspace_id: &str,
    message: &str,
    files: Vec<PathBuf>,
) -> Result<Kept> {
    let (id, message) = (workspace_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep(&id, &message, &files)).await?
}

/// Delete the copies of files that were refused, such as ones for a
/// workspace removed while they were made. Failure only logs: the workspace's
/// removal deletes whatever is left.
pub(crate) async fn discard(kept: Kept) {
    let discarded = tokio::task::spawn_blocking(move || kept.discard()).await;
    if let Err(error) = discarded
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        crate::daemon::log!("refused attached files not deleted: {error:#}");
    }
}

impl Manager {
    /// Hold the gate of the files attached to `workspace`, with
    /// where its repository keeps them, once the workspace is still there and
    /// not being removed. A registration older than the repository's
    /// directory reserves it first, under the Git gate, which is never taken
    /// while holding this one.
    pub(crate) async fn lock_files(
        &self,
        workspace: &Workspace,
    ) -> Result<(Layout, OwnedMutexGuard<()>)> {
        let repo = self.repository(&workspace.repository_id).await?;
        if repo.workspaces_dir.is_none() {
            let (repo, _git) = self.lock_repository(&repo.id).await?;
            self.workspaces_dir(&repo).await?;
        }
        let guard = self.gates.lock_message_files(&repo.id).await;
        // A move or removal may have finished while this waited.
        let current = self.workspace(&workspace.id).await?;
        ensure!(
            current.state != WorkspaceState::Removing,
            "the workspace is being removed"
        );
        let repo = self.repository(&repo.id).await?;
        let layout = Layout::of(&repo).context("the repository has no directory")?;
        Ok((layout, guard))
    }

    /// Delete the files attached to `workspace` once it is removed.
    pub(crate) async fn delete_workspace_files(&self, workspace: &Workspace) -> Result<()> {
        let _files = self
            .gates
            .lock_message_files(&workspace.repository_id)
            .await;
        let repo = self.repository(&workspace.repository_id).await?;
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
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || layout.remove()).await?
    }
}
