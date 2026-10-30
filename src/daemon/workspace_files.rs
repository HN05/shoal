//! Files attached to workspaces, such as those sent with a message to their
//! agents. Each repository keeps Shoal's copies beside its workspaces, in a
//! directory its agents are given as a working directory, and the store
//! records each copy with how it came, so `shoal files` lists them.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::{scope::Caller, workspace::Manager},
    model::{HandedBack, Repository, Workspace},
    state::{WorkspaceState, states},
};

mod earlier;
pub(crate) mod layout;
mod records;
mod retention;

pub(crate) use records::{forget_removed, rebase, record};

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

/// Who attached a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attacher {
    /// A request from outside any agent.
    User,
    /// A request from an agent's process tree.
    Agent {
        /// The agent, named as `--agent` names it.
        agent: String,
        /// The execution that ran the agent, which a restart replaces.
        execution: String,
        /// The session the agent reported, which outlives a resume; none
        /// before its first report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}

/// The most files one completion hands back, which keeps the completion that
/// lists their copies to one protocol frame.
pub const MAX_RESULTS: usize = 64;

/// Refuse more results than one completion hands back.
pub fn check_results(results: &[NewFile]) -> Result<()> {
    ensure!(
        results.len() <= MAX_RESULTS,
        "hand back at most {MAX_RESULTS} files with one completion"
    );
    Ok(())
}

/// The `names` and record `ids` that none of `files` has, as an error
/// names them.
pub fn missing_selection(files: &[AttachedFile], names: &[String], ids: &[i64]) -> Vec<String> {
    names
        .iter()
        .filter(|name| !files.iter().any(|file| file.name == **name))
        .cloned()
        .chain(
            ids.iter()
                .filter(|id| !files.iter().any(|file| file.id == **id))
                .map(|id| format!("number {id}")),
        )
        .collect()
}

/// A file to attach, by absolute path, with the title that says what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFile {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl From<PathBuf> for NewFile {
    fn from(path: PathBuf) -> Self {
        Self { path, title: None }
    }
}

impl NewFile {
    /// Refuse a title that is not a single line of text.
    pub fn check(&self) -> Result<()> {
        self.title
            .as_deref()
            .map_or(Ok(()), crate::validate::file_title)
    }
}

/// A file attached to a workspace: Shoal's copy and how it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: i64,
    /// The workspace's name when the file was attached.
    pub workspace: String,
    pub source: FileSource,
    /// The attached file's name.
    pub name: String,
    /// What the file is, when whoever attached it said; clients show the
    /// name otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Shoal's copy.
    pub path: PathBuf,
    pub bytes: u64,
    /// Unix seconds.
    pub created_at: i64,
    /// None for a file attached before Shoal recorded who attached it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_by: Option<Attacher>,
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
    files: Vec<NewFile>,
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
    /// Who a request from `caller` attaches files as: the agent it comes
    /// from, else the user.
    pub(crate) async fn attacher(&self, caller: Option<&Caller>) -> Attacher {
        let Some(agent) = (match caller {
            Some(caller) => self.caller_agent(caller).await,
            None => None,
        }) else {
            return Attacher::User;
        };
        Attacher::Agent {
            session: crate::execution::recovery::reported_session(
                &self.paths,
                &agent.workspace_id,
                &agent.execution,
            ),
            agent: agent.name,
            execution: agent.execution,
        }
    }

    /// The files attached to the workspace `selector` names, or that a
    /// removed workspace of that name handed back, oldest first.
    pub async fn workspace_files(&self, selector: &str) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        self.store.run(move |db| records::list(db, &owner.id)).await
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
    /// completion, recorded as its results attached by `attacher`, which
    /// outlive its removal.
    pub(crate) async fn keep_results(
        &self,
        workspace: &Workspace,
        results: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<HandedBack>> {
        if results.is_empty() {
            return Ok(Vec::new());
        }
        let (layout, _files) = self.lock_files(workspace).await?;
        let id = workspace.id.clone();
        let kept =
            tokio::task::spawn_blocking(move || layout.keep_results(&id, &results)).await??;
        let records = self
            .record_or_discard(&workspace.id, FileSource::Result, attacher, kept)
            .await?;
        Ok(records
            .into_iter()
            .map(|file| HandedBack {
                id: file.id,
                name: file.name,
            })
            .collect())
    }

    /// Delete the files attached to the workspace `selector` names, only the
    /// copies of files with one of `names` and the records `ids` when there
    /// are any, and return their records. A name or record the workspace does
    /// not have deletes nothing.
    pub async fn remove_workspace_files(
        &self,
        selector: &str,
        names: Vec<String>,
        ids: Vec<i64>,
    ) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        let (repo, _files) = self.lock_repository_files(&owner.repository_id).await?;
        let id = owner.id.clone();
        let mut files = self.store.run(move |db| records::list(db, &id)).await?;
        if !names.is_empty() || !ids.is_empty() {
            let missing = missing_selection(&files, &names, &ids);
            ensure!(
                missing.is_empty(),
                "workspace {} has no attached file {}",
                owner.name,
                missing.join(", ")
            );
            files.retain(|file| names.contains(&file.name) || ids.contains(&file.id));
        }
        self.delete_files(&repo, files).await
    }

    /// Delete the attached files of every workspace without a running agent
    /// or command, or one whose end is unresolved, and return their records.
    /// No execution starts while a repository's files are chosen and deleted.
    pub async fn remove_idle_files(&self) -> Result<Vec<AttachedFile>> {
        let mut deleted = Vec::new();
        for listed in self.repositories().await? {
            // Execution admission comes first, as nothing holding the files
            // gate waits for it.
            let _admission = self.executions.connections().await;
            let (repo, _files) = self.lock_repository_files(&listed.id).await?;
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
    /// message, as `attacher`, and return their records. Adding the same
    /// files again keeps the copies they have, with any new title.
    pub async fn add_workspace_files(
        &self,
        selector: &str,
        files: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<AttachedFile>> {
        ensure!(!files.is_empty(), "give the files to add");
        let workspace = self.workspace(selector).await?;
        let (layout, _files) = self.lock_files(&workspace).await?;
        let kept = keep(layout, &workspace.id, ADDED, files).await?;
        self.record_or_discard(&workspace.id, FileSource::Added, attacher, kept)
            .await
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source` by `attacher` and return their records, or delete the
    /// copies made for them when that fails.
    async fn record_or_discard(
        &self,
        workspace_id: &str,
        source: FileSource,
        attacher: &Attacher,
        kept: Kept,
    ) -> Result<Vec<AttachedFile>> {
        let recorded = self
            .record_kept(workspace_id, source, attacher, &kept)
            .await;
        if recorded.is_err() {
            discard(kept).await;
        }
        recorded
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source` by `attacher`, and return their records.
    async fn record_kept(
        &self,
        workspace_id: &str,
        source: FileSource,
        attacher: &Attacher,
        kept: &Kept,
    ) -> Result<Vec<AttachedFile>> {
        let (id, attacher) = (workspace_id.to_owned(), attacher.clone());
        let copies = kept.copies.clone();
        self.store
            .run(move |db| records::record_all(db, &id, source, &attacher, &copies))
            .await
    }

    /// Hold the gate of repository `repository_id`'s attached files, with the
    /// repository as it is once held: a move of its directory, which holds
    /// the gate, may have finished meanwhile.
    pub(crate) async fn lock_repository_files(
        &self,
        repository_id: &str,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let guard = self.gates.lock_message_files(repository_id).await;
        let repo = self.repository(repository_id).await?;
        Ok((repo, guard))
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
        let repo = self
            .reserve_repository_directory(&workspace.repository_id)
            .await?;
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
