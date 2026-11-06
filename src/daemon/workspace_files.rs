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

pub(crate) use records::{
    count, forget_removed, ids_at, keep_ended_chat, rebase, record, sent_files, sent_ids,
};

use layout::{Kept, Layout};

pub(crate) use crate::client::files::{
    AttachedFile, Attacher, FileSource, NewFile, SentFile, check_path, check_results, check_sent,
    missing_selection,
};
#[cfg(test)]
mod tests;

/// The space, a workspace or chat, whose attached files a request names:
/// one that exists, or a removed workspace or removed chat whose files are
/// kept.
#[derive(Debug, Clone)]
pub(crate) struct Space {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The repository whose directory keeps its files; none for a chat
    /// without a repository, whose files `root_dir` keeps.
    pub(crate) repository_id: Option<String>,
    /// The `root_dir` a chat without a repository keeps its files in, the
    /// one it started in; none for a space of a repository.
    pub(crate) root_dir: Option<PathBuf>,
    /// The space's parent when it attached the files.
    pub(crate) parent_id: Option<String>,
}

/// Which home keeps attached files: a repository's directory, or a
/// `root_dir` that keeps those of chats without a repository.
#[derive(Debug, Clone)]
pub(crate) enum HomeKey {
    Repository(Repository),
    Root(PathBuf),
}

impl HomeKey {
    /// Its repository's ID; none for a `root_dir`.
    pub(crate) fn repository_id(&self) -> Option<String> {
        match self {
            Self::Repository(repo) => Some(repo.id.clone()),
            Self::Root(_) => None,
        }
    }

    /// Its `root_dir` as records name it; none for a repository.
    pub(crate) fn root_dir(&self) -> Option<String> {
        match self {
            Self::Repository(_) => None,
            Self::Root(root) => Some(root.to_string_lossy().into_owned()),
        }
    }

    /// What a failure names it by.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Repository(repo) => repo.path.display().to_string(),
            Self::Root(root) => format!("the chats without a repository in {}", root.display()),
        }
    }
}

/// Where attached files are kept: in a repository's directory, or for the
/// chats without a repository, in `root_dir`.
pub(crate) enum FilesHome {
    Repository(Repository),
    Outside(Layout),
}

impl FilesHome {
    /// Where its files are; a repository registered before the repository
    /// directory layout has none.
    pub(crate) fn layout(&self) -> Option<Layout> {
        match self {
            Self::Repository(repo) => Layout::of(repo),
            Self::Outside(layout) => Some(layout.clone()),
        }
    }
}

/// The key of the gate of the files of the chats without a repository,
/// which no repository ID takes.
const OUTSIDE_GATE: &str = "";

impl From<Workspace> for Space {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            repository_id: Some(workspace.repository_id),
            root_dir: None,
            parent_id: workspace.parent.map(|parent| parent.id),
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
            root_dir: chat.root_dir,
            parent_id: chat.parent.map(|parent| parent.id),
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

/// Copies of `files` that the workspace or chat `space_id` hands the user
/// with `message`, apart from its other files since they outlive its
/// removal, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
async fn copy_for_user(
    layout: Layout,
    space_id: &str,
    message: &str,
    files: Vec<NewFile>,
) -> Result<Kept> {
    let (id, message) = (space_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep_results(&id, &message, &files)).await?
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
    /// from, the chat whose agent's scope it carries, else the running chat
    /// `sender_chat` whose agent, started before chats carried scope, sends
    /// it unscoped, else the user.
    pub(crate) async fn attacher(
        &self,
        caller: Option<&Caller>,
        sender_chat: Option<&str>,
    ) -> Attacher {
        let Some(caller) = caller else {
            return self.chat_attacher(sender_chat).await;
        };
        if self.caller_is_chat(caller).await.unwrap_or_default() {
            return self.chat_attacher(Some(&caller.space_id)).await;
        }
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
    /// a chat that is stopped or a request from outside any chat.
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
    /// kept for a removed space of that name, oldest first.
    pub async fn attached_files(&self, selector: &str) -> Result<Vec<AttachedFile>> {
        let space = self.file_space(selector).await?;
        self.store.run(move |db| records::list(db, &space.id)).await
    }

    /// The workspace or chat `selector` names, or else the latest removed
    /// space of that name or ID whose files are kept.
    pub(crate) async fn file_space(&self, selector: &str) -> Result<Space> {
        let error = match self.present_space(selector).await {
            Ok(space) => return Ok(space.into()),
            Err(error) => error,
        };
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
        let kept = |files: Vec<SentFile>| std::future::ready(Ok(files));
        let records = (self.send_with_files(
            space,
            FileSource::Result,
            "shoal done",
            results,
            attacher,
            kept,
        ))
        .await?;
        Ok(records
            .into_iter()
            .map(|file| HandedBack {
                id: file.id,
                name: file.name,
            })
            .collect())
    }

    /// Hand the user `message`, such as an alert, a question or a
    /// completion, from `space` with `send`, passing it copies of the files
    /// `files` recorded as coming from `source` and attached by `attacher`,
    /// which outlive its removal. When `send` refuses the message, the copies
    /// and records made for it go; those an identical earlier message made
    /// stay with it.
    pub(crate) async fn send_with_files<T, Send, Sent>(
        &self,
        space: &Space,
        source: FileSource,
        message: &str,
        files: Vec<NewFile>,
        attacher: &Attacher,
        send: Send,
    ) -> Result<T>
    where
        Send: FnOnce(Vec<SentFile>) -> Sent,
        Sent: Future<Output = Result<T>>,
    {
        if files.is_empty() {
            return send(Vec::new()).await;
        }
        let (layout, _files) = self.lock_files(space).await?;
        let kept = copy_for_user(layout, &space.id, message, files).await?;
        let records = self
            .record_or_discard(&space.id, source, attacher, kept.clone())
            .await?;
        let sent = send(records.iter().map(SentFile::from).collect()).await;
        if sent.is_err() {
            self.forget_unsent(kept, records).await;
        }
        sent
    }

    /// Delete the copies `kept` and their records `records` once the message
    /// they were made for was refused, unless the copies were reused.
    /// Failure only logs, as the message's refusal is what its sender hears.
    async fn forget_unsent(&self, kept: Kept, records: Vec<AttachedFile>) {
        if kept.reused() {
            return;
        }
        let ids: Vec<i64> = records.iter().map(|file| file.id).collect();
        if let Err(error) = self.store.run(move |db| records::forget(db, &ids)).await {
            crate::daemon::log!("records of refused files not deleted: {error:#}");
        }
        discard(kept).await;
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
        let (home, _files) = self
            .lock_files_home(&self.space_home(&space).await?)
            .await?;
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
        self.delete_files(&home, files).await
    }

    /// Delete the attached files of every workspace without a running agent
    /// or command, or one whose end is unresolved, and of every removed
    /// workspace and removed chat, and return their records.
    /// No execution starts while a repository's files are chosen and deleted.
    pub async fn remove_idle_files(&self) -> Result<Vec<AttachedFile>> {
        let mut deleted = Vec::new();
        for key in self.files_homes().await? {
            // Execution admission comes first, as nothing holding the files
            // gate waits for it.
            let _admission = self.executions.connections().await;
            let (home, _files) = self.lock_files_home(&key).await?;
            let files = self.store.run(move |db| records::idle(db, &key)).await?;
            deleted.extend(self.delete_files(&home, files).await?);
        }
        Ok(deleted)
    }

    /// The homes of attached files: every repository's directory, and each
    /// `root_dir` that keeps files of chats without a repository.
    pub(crate) async fn files_homes(&self) -> Result<Vec<HomeKey>> {
        let repositories = self.repositories().await?.into_iter();
        let roots = self.store.run(|db| records::roots(db)).await?;
        Ok((repositories.map(HomeKey::Repository))
            .chain(roots.into_iter().map(|root| HomeKey::Root(root.into())))
            .collect())
    }

    /// Delete the copies `files` kept in `home` and then their records.
    /// Callers hold the gate of its attached files.
    async fn delete_files(
        &self,
        home: &FilesHome,
        files: Vec<AttachedFile>,
    ) -> Result<Vec<AttachedFile>> {
        let layout = home.layout().context("the repository has no directory")?;
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

    /// Where the chats without a repository keep their attached files in
    /// `root_dir`, marked as this daemon's.
    pub(crate) fn outside_files_layout(&self, root_dir: &std::path::Path) -> Layout {
        Layout::outside_repositories(root_dir, &self.root_dir_owner())
    }

    /// The home that keeps the files of `space`: its repository's, or the
    /// `root_dir` a chat without one started in, or else the configured one.
    pub(crate) async fn space_home(&self, space: &Space) -> Result<HomeKey> {
        Ok(match (&space.repository_id, &space.root_dir) {
            (Some(id), _) => HomeKey::Repository(self.repository(id).await?),
            (None, Some(root_dir)) => HomeKey::Root(root_dir.clone()),
            (None, None) => HomeKey::Root(self.config().root_dir(&self.paths)?),
        })
    }

    /// The names of the chats without a repository whose files are kept.
    pub(crate) async fn outside_files_kept(&self) -> Result<Vec<String>> {
        self.store.run(|db| records::outside_repositories(db)).await
    }

    /// Hold the gate of the attached files `key` keeps, with where they are
    /// once held: a move of a repository's directory, which holds the gate,
    /// may have finished meanwhile.
    pub(crate) async fn lock_files_home(
        &self,
        key: &HomeKey,
    ) -> Result<(FilesHome, OwnedMutexGuard<()>)> {
        let gate = match key {
            HomeKey::Repository(repo) => repo.id.clone(),
            HomeKey::Root(_) => OUTSIDE_GATE.to_owned(),
        };
        let guard = self.gates.lock_message_files(&gate).await;
        let home = match key {
            HomeKey::Repository(repo) => FilesHome::Repository(self.repository(&repo.id).await?),
            HomeKey::Root(root_dir) => FilesHome::Outside(self.outside_files_layout(root_dir)),
        };
        Ok((home, guard))
    }

    /// Hold the gate of the files attached to the workspace or chat `space`,
    /// with where they are kept, once the workspace is still there and not
    /// being removed, or the chat still runs. A registration older than the
    /// repository's directory reserves it first, under the Git gate, which
    /// is never taken while holding this one.
    pub(crate) async fn lock_files(&self, space: &Space) -> Result<(Layout, OwnedMutexGuard<()>)> {
        if let Some(repository) = &space.repository_id {
            self.reserve_repository_directory(repository).await?;
        }
        let (home, guard) = self.lock_files_home(&self.space_home(space).await?).await?;
        // A move, removal or end may have finished while this waited.
        self.ensure_attachable(&space.id).await?;
        let layout = home.layout().context("the repository has no directory")?;
        Ok((layout, guard))
    }

    /// Refuse attaching files to the space `space_id` once the
    /// workspace is being removed or is gone, or the chat was removed.
    async fn ensure_attachable(&self, space_id: &str) -> Result<()> {
        match self.workspace(space_id).await {
            Ok(workspace) => ensure!(
                workspace.state != WorkspaceState::Removing,
                "the workspace is being removed"
            ),
            Err(error) if self.recorded_chat(space_id).await?.is_none() => return Err(error),
            Err(_) => {}
        }
        Ok(())
    }
}
