//! Files attached to workspaces, such as those sent with a message to their
//! agents. Each repository keeps Shoal's copies beside its workspaces, in a
//! directory its agents are given as a working directory, and the store
//! records each copy with how it came, so `shoal files` lists them.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
    state::{WorkspaceState, states},
};

pub(crate) mod layout;
mod records;
mod retention;

pub(crate) use records::{rebase, record};

use layout::{Kept, Layout};

#[cfg(test)]
mod tests;

states!(FileSource {
    /// Sent with a message to the workspace's agents.
    Message => "message",
    /// Added with `shoal files add`.
    Added => "added",
    /// Handed back with `shoal done`; kept after the workspace's removal for
    /// as long as its removal record.
    Result => "result",
});

/// A file attached to a workspace: Shoal's copy and how it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: i64,
    /// The workspace's name when the file was attached.
    pub workspace: String,
    pub source: FileSource,
    /// The attached file's name.
    pub name: String,
    /// Shoal's copy.
    pub path: PathBuf,
    pub bytes: u64,
    /// Unix seconds.
    pub created_at: i64,
    /// When a removed workspace's file is deleted, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// The workspace whose attached files a request names: one that exists, or
/// a removed one whose results are kept.
#[derive(Debug, Clone)]
pub(crate) struct Owner {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) repository_id: String,
    /// The workspace's parent when it attached the files.
    pub(crate) parent_id: Option<String>,
}

impl From<Workspace> for Owner {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            repository_id: workspace.repository_id,
            parent_id: workspace.parent_workspace.map(|parent| parent.id),
        }
    }
}

/// The text copies of added files are kept under, which tells them from a
/// message that is only its files.
const ADDED: &str = "shoal files add";

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
    /// The files attached to the workspace `selector` names, or that a
    /// removed workspace of that name handed back, oldest first.
    pub async fn workspace_files(&self, selector: &str) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        self.store
            .run(move |db| records::list(db, &owner.id, None))
            .await
    }

    /// The workspace `selector` names, or else the latest removed one of
    /// that name or ID whose results are kept.
    pub(crate) async fn file_owner(&self, selector: &str) -> Result<Owner> {
        let error = match self.workspace(selector).await {
            Ok(workspace) => return Ok(workspace.into()),
            Err(error) => error,
        };
        let selector = selector.to_owned();
        let removed = self
            .store
            .run(move |db| records::removed_owner(db, &selector))
            .await?;
        removed.ok_or(error)
    }

    /// Copies of the files `results` that `workspace` hands back with its
    /// completion, recorded as its results, which outlive its removal.
    pub(crate) async fn keep_results(
        &self,
        workspace: &Workspace,
        results: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>> {
        if results.is_empty() {
            return Ok(results);
        }
        let (layout, _files) = self.lock_files(workspace).await?;
        let id = workspace.id.clone();
        let kept =
            tokio::task::spawn_blocking(move || layout.keep_results(&id, &results)).await??;
        let records = self
            .record_or_discard(&workspace.id, FileSource::Result, kept)
            .await?;
        Ok(records.into_iter().map(|file| file.path).collect())
    }

    /// Delete the files attached to the workspace `selector` names, only the
    /// copies of files called `name` when given, and return their records.
    pub async fn remove_workspace_files(
        &self,
        selector: &str,
        name: Option<String>,
    ) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        let repo = self.repository(&owner.repository_id).await?;
        let _files = self.gates.lock_message_files(&repo.id).await;
        let (id, wanted) = (owner.id.clone(), name.clone());
        let files = self
            .store
            .run(move |db| records::list(db, &id, wanted.as_deref()))
            .await?;
        if let Some(name) = &name {
            ensure!(
                !files.is_empty(),
                "workspace {} has no attached file named {name}",
                owner.name
            );
        }
        self.delete_files(&repo, files).await
    }

    /// Delete the attached files of every workspace without a running agent
    /// or command, or one whose end is unresolved, and return their records.
    /// No execution starts while a repository's files are chosen and deleted.
    pub async fn remove_idle_files(&self) -> Result<Vec<AttachedFile>> {
        let mut deleted = Vec::new();
        for repo in self.repositories().await? {
            // Execution admission comes first, as nothing holding the files
            // gate waits for it.
            let _admission = self.executions.connections().await;
            let _files = self.gates.lock_message_files(&repo.id).await;
            let id = repo.id.clone();
            let files = self.store.run(move |db| records::idle(db, &id)).await?;
            deleted.extend(self.delete_files(&repo, files).await?);
        }
        Ok(deleted)
    }

    /// Delete the copies `files` of `repo` and then their records. Callers
    /// hold the gate of the repository's attached files.
    async fn delete_files(
        &self,
        repo: &Repository,
        files: Vec<AttachedFile>,
    ) -> Result<Vec<AttachedFile>> {
        let layout = Layout::of(repo).context("the repository has no directory")?;
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
        tokio::task::spawn_blocking(move || {
            paths.iter().try_for_each(|path| layout.delete_copy(path))
        })
        .await??;
        let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
        self.store.run(move |db| records::forget(db, &ids)).await?;
        Ok(files)
    }

    /// Attach copies of `files` to the workspace `selector` names, without a
    /// message, and return their records. Adding the same files again keeps
    /// the copies they have.
    pub async fn add_workspace_files(
        &self,
        selector: &str,
        files: Vec<PathBuf>,
    ) -> Result<Vec<AttachedFile>> {
        ensure!(!files.is_empty(), "give the files to add");
        let workspace = self.workspace(selector).await?;
        let (layout, _files) = self.lock_files(&workspace).await?;
        let kept = keep(layout, &workspace.id, ADDED, files).await?;
        self.record_or_discard(&workspace.id, FileSource::Added, kept)
            .await
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source` and return their records, or delete the copies made for
    /// them when that fails.
    async fn record_or_discard(
        &self,
        workspace_id: &str,
        source: FileSource,
        kept: Kept,
    ) -> Result<Vec<AttachedFile>> {
        let recorded = self.record_kept(workspace_id, source, &kept).await;
        if recorded.is_err() {
            discard(kept).await;
        }
        recorded
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source`, and return their records.
    async fn record_kept(
        &self,
        workspace_id: &str,
        source: FileSource,
        kept: &Kept,
    ) -> Result<Vec<AttachedFile>> {
        let id = workspace_id.to_owned();
        let (paths, bytes) = (kept.paths.clone(), kept.bytes.clone());
        self.store
            .run(move |db| records::record_all(db, &id, source, &paths, &bytes))
            .await
    }

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
}
