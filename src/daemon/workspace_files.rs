//! Files attached to workspaces and chats, such as those sent with a message
//! to a workspace's agents. Each repository keeps Shoal's copies beside its
//! workspaces, in a directory its agents are given as a working directory,
//! and the store records each copy with how it came, so `shoal files` lists
//! them.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::{scope::Caller, workspace::Manager},
    model::{Chat, HandedBack, PresentSpace, Repository, Workspace},
    state::WorkspaceState,
};

mod earlier;
pub(crate) mod layout;
mod records;
mod retention;

pub(crate) use records::{forget_removed, keep_ended_chat, rebase, record};

use layout::{Kept, Layout};

pub(crate) use crate::client::files::{
    AttachedFile, Attacher, FileSource, NewFile, check_results, missing_selection,
};
#[cfg(test)]
mod tests;

/// The space, a workspace or chat, whose attached files a request names:
/// one that exists, or a removed workspace or ended chat whose files are
/// kept.
#[derive(Debug, Clone)]
pub(crate) struct Space {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) repository_id: String,
    /// The workspace's parent when it attached the files; a chat has none.
    pub(crate) parent_id: Option<String>,
}

impl From<Workspace> for Space {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            repository_id: workspace.repository_id,
            parent_id: workspace.parent_workspace.map(|parent| parent.id),
        }
    }
}

impl From<PresentSpace> for Space {
    fn from(space: PresentSpace) -> Self {
        match space {
            PresentSpace::Workspace(workspace) => workspace.into(),
            PresentSpace::Chat(chat) => chat.into(),
        }
    }
}

impl From<Chat> for Space {
    fn from(chat: Chat) -> Self {
        Self {
            id: chat.id,
            name: chat.name,
            repository_id: chat.repository_id,
            parent_id: None,
        }
    }
}

/// The text copies of added files are kept under, which tells them from a
/// message that is only its files.
const ADDED: &str = "shoal files add";

/// Copies of `files`, attached to `message` for the workspace or chat
/// `space_id`, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
pub(crate) async fn keep(
    layout: Layout,
    space_id: &str,
    message: &str,
    files: Vec<NewFile>,
) -> Result<Kept> {
    let (id, message) = (space_id.to_owned(), message.to_owned());
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
    /// Copy `attachments` into the files of `space` and queue `message` with
    /// the copies through `queue`, deleting the copies made for a message it
    /// refuses.
    pub(crate) async fn queue_attached<Queue, Queued>(
        &self,
        space: Space,
        message: &str,
        attachments: Vec<NewFile>,
        queue: Queue,
    ) -> Result<()>
    where
        Queue: FnOnce(Vec<layout::Copy>) -> Queued,
        Queued: Future<Output = Result<()>>,
    {
        let (layout, _files) = self.lock_files(&space).await?;
        let kept = keep(layout, &space.id, message, attachments).await?;
        let queued = queue(kept.copies.clone()).await;
        if queued.is_err() {
            discard(kept).await;
        }
        queued
    }

    /// Who a request from `caller` attaches files as: the agent it comes
    /// from, else the running chat `sender_chat` whose agent sends it
    /// outside any workspace scope, else the user.
    pub(crate) async fn attacher(
        &self,
        caller: Option<&Caller>,
        sender_chat: Option<&str>,
    ) -> Attacher {
        let Some(caller) = caller else {
            return self.chat_attacher(sender_chat).await;
        };
        let Some(agent) = self.caller_agent(caller).await else {
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

    /// The running chat `sender_chat` as an attacher, else the user, as for
    /// a chat that has ended or a request from outside any chat.
    async fn chat_attacher(&self, sender_chat: Option<&str>) -> Attacher {
        let running = match sender_chat {
            Some(chat) => self.running_chat(chat).await,
            None => return Attacher::User,
        };
        match running {
            Ok(Some(chat)) => Attacher::Chat {
                agent: chat.agent,
                chat: chat.id,
            },
            Ok(None) => Attacher::User,
            Err(error) => {
                crate::daemon::log!("attaching chat unknown: {error:#}");
                Attacher::User
            }
        }
    }

    /// The files attached to the workspace or chat `selector` names, or
    /// kept for a removed workspace or ended chat of that name, oldest first.
    pub async fn attached_files(&self, selector: &str) -> Result<Vec<AttachedFile>> {
        let space = self.file_space(selector).await?;
        self.store.run(move |db| records::list(db, &space.id)).await
    }

    /// The workspace or running chat `selector` names, else a saved chat,
    /// which keeps its files until it ends, or else the latest removed
    /// workspace or ended chat of that name or ID whose files are kept.
    pub(crate) async fn file_space(&self, selector: &str) -> Result<Space> {
        let error = match self.present_space(selector).await {
            Ok(space) => return Ok(space.into()),
            Err(error) => error,
        };
        if let Some(saved) = self.recorded_chat(selector).await? {
            return Ok(crate::model::PresentSpace::Chat(saved).into());
        }
        let selector = selector.to_owned();
        let gone = self
            .store
            .run(move |db| records::gone_space(db, &selector))
            .await?;
        gone.ok_or(error)
    }

    /// Copies of the files `results` that `space` hands back with its
    /// completion, recorded as its results attached by `attacher`, which
    /// outlive its removal.
    pub(crate) async fn keep_results(
        &self,
        space: &Space,
        results: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<HandedBack>> {
        if results.is_empty() {
            return Ok(Vec::new());
        }
        let (layout, _files) = self.lock_files(space).await?;
        let id = space.id.clone();
        let kept =
            tokio::task::spawn_blocking(move || layout.keep_results(&id, &results)).await??;
        let records = self
            .record_or_discard(&space.id, FileSource::Result, attacher, kept)
            .await?;
        Ok(records
            .into_iter()
            .map(|file| HandedBack {
                id: file.id,
                name: file.name,
            })
            .collect())
    }

    /// Delete the files attached to the workspace or chat `selector` names,
    /// only the copies of files with one of `names` and the records `ids`
    /// when there are any, and return their records. A name or record it does
    /// not have deletes nothing.
    pub async fn remove_attached_files(
        &self,
        selector: &str,
        names: Vec<String>,
        ids: Vec<i64>,
    ) -> Result<Vec<AttachedFile>> {
        let space = self.file_space(selector).await?;
        let (repo, _files) = self.lock_repository_files(&space.repository_id).await?;
        let id = space.id.clone();
        let mut files = self.store.run(move |db| records::list(db, &id)).await?;
        if !names.is_empty() || !ids.is_empty() {
            let missing = missing_selection(&files, &names, &ids);
            ensure!(
                missing.is_empty(),
                "{} has no attached file {}",
                space.name,
                missing.join(", ")
            );
            files.retain(|file| names.contains(&file.name) || ids.contains(&file.id));
        }
        self.delete_files(&repo, files).await
    }

    /// Delete the attached files of every workspace without a running agent
    /// or command, or one whose end is unresolved, and of every removed
    /// workspace and ended chat, and return their records.
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

    /// Attach copies of `files` to the workspace or running chat `selector`
    /// names, without a message, as `attacher`, and return their records.
    /// Adding the same files again keeps the copies they have, with any new
    /// title.
    pub async fn add_attached_files(
        &self,
        selector: &str,
        files: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<AttachedFile>> {
        ensure!(!files.is_empty(), "give the files to add");
        let space: Space = self.present_space(selector).await?.into();
        let (layout, _files) = self.lock_files(&space).await?;
        let kept = keep(layout, &space.id, ADDED, files).await?;
        self.record_or_discard(&space.id, FileSource::Added, attacher, kept)
            .await
    }

    /// Record the copies `kept` as attached to the workspace or chat
    /// `space_id` from `source` by `attacher` and return their records, or
    /// delete the copies made for them when that fails.
    async fn record_or_discard(
        &self,
        space_id: &str,
        source: FileSource,
        attacher: &Attacher,
        kept: Kept,
    ) -> Result<Vec<AttachedFile>> {
        let recorded = self.record_kept(space_id, source, attacher, &kept).await;
        if recorded.is_err() {
            discard(kept).await;
        }
        recorded
    }

    /// Record the copies `kept` as attached to the workspace or chat
    /// `space_id` from `source` by `attacher`, and return their records.
    async fn record_kept(
        &self,
        space_id: &str,
        source: FileSource,
        attacher: &Attacher,
        kept: &Kept,
    ) -> Result<Vec<AttachedFile>> {
        let (id, attacher) = (space_id.to_owned(), attacher.clone());
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

    /// Hold the gate of the files attached to the workspace or chat `space`,
    /// with where its repository keeps them, once the workspace is still
    /// there and not being removed, or the chat still runs. A registration
    /// older than the repository's directory reserves it first, under the Git
    /// gate, which is never taken while holding this one.
    pub(crate) async fn lock_files(&self, space: &Space) -> Result<(Layout, OwnedMutexGuard<()>)> {
        let repo = self
            .reserve_repository_directory(&space.repository_id)
            .await?;
        let guard = self.gates.lock_message_files(&repo.id).await;
        // A move, removal or end may have finished while this waited.
        self.ensure_attachable(&space.id).await?;
        let repo = self.repository(&repo.id).await?;
        let layout = Layout::of(&repo).context("the repository has no directory")?;
        Ok((layout, guard))
    }

    /// Refuse attaching files to the space `space_id` once the
    /// workspace is being removed or is gone, or the chat has ended.
    async fn ensure_attachable(&self, space_id: &str) -> Result<()> {
        match self.workspace(space_id).await {
            Ok(workspace) => ensure!(
                workspace.state != WorkspaceState::Removing,
                "the workspace is being removed"
            ),
            Err(error) if self.running_chat(space_id).await?.is_none() => return Err(error),
            Err(_) => {}
        }
        Ok(())
    }
}
