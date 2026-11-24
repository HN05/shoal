//! Files attached to workspaces and chats, such as those sent with a message
//! to a workspace's agents. Each repository keeps Shoal's copies beside its
//! workspaces, in a directory its agents are given as a working directory,
//! and the store records each copy with how it came, so `shoal files` lists
//! them.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::ByKind;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use futures_util::future::BoxFuture;
use shoal_coordination::daemon::{Attaching, Unsent};
use tokio::sync::OwnedMutexGuard;

use crate::{
    model::{Chat, HandedBack, Repository, Workspace},
    scope::Caller,
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

pub mod fetched;
pub mod layout;
mod reading;
mod records;
mod retention;

pub use records::{claim, count, keep_removed, kept_ids, rebase, record, restore};

use layout::{Kept, Layout};

pub use shoal_protocol::files::{
    AttachedFile, AttachedTo, Attacher, FileSource, KeptCopy, NewFile, SentFile, SessionCopy,
    check_path, check_sent, missing_selection,
};

/// The space, a workspace or chat, whose attached files a request names:
/// one that exists, or a removed workspace or removed chat whose files are
/// kept.
#[derive(Debug, Clone)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    /// The repository whose directory keeps its files; none for a chat
    /// without a repository, whose files `root_dir` keeps.
    pub repository_id: Option<RepositoryId>,
    /// The `root_dir` a chat without a repository keeps its files in, the
    /// one it started in; none for a space of a repository.
    pub root_dir: Option<PathBuf>,
    /// The space's parent when it attached the files.
    pub parent_id: Option<SpaceId>,
}

/// Which home keeps attached files: a repository's directory, or a
/// `root_dir` that keeps those of chats without a repository.
#[derive(Debug, Clone)]
pub enum HomeKey {
    Repository(Repository),
    Root(PathBuf),
}

impl HomeKey {
    /// Its repository's ID; none for a `root_dir`.
    pub fn repository_id(&self) -> Option<RepositoryId> {
        match self {
            Self::Repository(repo) => Some(repo.id.clone()),
            Self::Root(_) => None,
        }
    }

    /// Its `root_dir` as records name it; none for a repository.
    pub fn root_dir(&self) -> Option<String> {
        match self {
            Self::Repository(_) => None,
            Self::Root(root) => Some(root.to_string_lossy().into_owned()),
        }
    }

    /// What a failure names it by.
    pub fn describe(&self) -> String {
        match self {
            Self::Repository(repo) => repo.path.display().to_string(),
            Self::Root(root) => format!("the chats without a repository in {}", root.display()),
        }
    }
}

/// Where attached files are kept: in a repository's directory, or for the
/// chats without a repository, in `root_dir`.
pub enum FilesHome {
    Repository(Repository),
    Outside(Layout),
}

impl FilesHome {
    /// Where its files are.
    pub fn layout(&self) -> Layout {
        match self {
            Self::Repository(repo) => Layout::of(repo),
            Self::Outside(layout) => layout.clone(),
        }
    }
}

impl From<Workspace> for Space {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name.into(),
            repository_id: workspace.repository_id,
            root_dir: None,
            parent_id: workspace.parent.map(|parent| parent.id),
        }
    }
}

impl From<crate::model::Space> for Space {
    fn from(space: crate::model::Space) -> Self {
        match space.by_kind() {
            ByKind::Workspace(workspace) => workspace.into(),
            ByKind::Chat(chat) => chat.into(),
        }
    }
}

impl From<Chat> for Space {
    fn from(chat: Chat) -> Self {
        Self {
            id: chat.id,
            name: chat.name.into(),
            repository_id: chat.repository_id,
            root_dir: chat.workdir.root_dir().map(Into::into),
            parent_id: chat.parent.map(|parent| parent.id),
        }
    }
}

/// The text copies of added files are kept under, which tells them from a
/// message that is only its files.
const ADDED: &str = "shoal files add";

/// The text copies of a handoff's session files are kept under.
const HANDOFF: &str = "shoal convert agent";

/// Copies of `files`, attached to `message` for the workspace or chat
/// `space_id`, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
pub async fn keep(
    layout: Layout,
    space_id: &SpaceId,
    message: &str,
    files: Vec<NewFile>,
) -> Result<Kept> {
    let (id, message) = (space_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep(&id, &message, &files)).await?
}

/// Copies of `files` that the workspace or chat `space_id` hands the user
/// with `message`, among its results, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
async fn copy_for_user(
    layout: Layout,
    space_id: &SpaceId,
    message: &str,
    files: Vec<NewFile>,
) -> Result<Kept> {
    let (id, message) = (space_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep_results(&id, &message, &files)).await?
}

/// Delete the copies of files that were refused, such as ones for a
/// workspace removed while they were made. Failure only logs, as the
/// refusal is what the sender hears.
pub async fn discard(kept: Kept) {
    let discarded = tokio::task::spawn_blocking(move || kept.discard()).await;
    if let Err(error) = discarded
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        crate::log!("refused attached files not deleted: {error:#}");
    }
}

impl Spaces {
    /// Copy `attachments` into the files of `space` and queue `message` with
    /// the copies through `queue`, deleting the copies made for a message it
    /// refuses.
    pub async fn queue_attached<Queue, Queued, T>(
        &self,
        daemon: &dyn Daemon,
        space: Space,
        message: &str,
        attachments: Vec<NewFile>,
        queue: Queue,
    ) -> Result<T>
    where
        Queue: FnOnce(Vec<layout::Copy>) -> Queued,
        Queued: Future<Output = Result<T>>,
    {
        let (layout, _files) = self.lock_files(daemon, &space).await?;
        let kept = keep(layout, &space.id, message, attachments).await?;
        let queued = queue(kept.copies.clone()).await;
        if queued.is_err() {
            discard(kept).await;
        }
        queued
    }

    /// Who a request from `caller` attaches files as: the agent it comes
    /// from, whatever its space, else the user.
    pub async fn attacher(&self, daemon: &dyn Daemon, caller: Option<&Caller>) -> Attacher {
        let agent = match caller {
            Some(caller) => daemon.caller_agent(caller).await,
            None => None,
        };
        let Some(agent) = agent else {
            return Attacher::User;
        };
        Attacher::Agent {
            session: crate::execution::recovery::carried::continued(
                &self.paths,
                &agent.space_id,
                &agent.execution,
            )
            .session,
            agent: agent.name,
            execution: agent.execution,
        }
    }

    /// The files attached to the workspace or chat `selector` names, or
    /// kept for the removed space of that ID, oldest first.
    pub async fn attached_files(&self, selector: &SpaceSelector) -> Result<Vec<AttachedFile>> {
        let space = self.file_space(selector).await?;
        self.store.run(move |db| records::list(db, &space.id)).await
    }

    /// The records of the copies at `paths`, which this machine keeps, as
    /// an alert sends them.
    pub async fn sent_files(&self, paths: Vec<PathBuf>) -> Result<Vec<SentFile>> {
        let records = self.store.read(move |db| records::at(db, &paths)).await?;
        Ok(records.iter().map(SentFile::from).collect())
    }

    /// The records of `files`, which this machine keeps, by their space and
    /// ID, with their copies' paths, in their order; a file whose record is
    /// gone drops out.
    pub async fn kept_sent_files(&self, files: Vec<SentFile>) -> Result<Vec<SentFile>> {
        let records = self.store.read(move |db| records::sent(db, &files)).await?;
        Ok(records.iter().map(SentFile::from).collect())
    }

    /// The workspace or chat `selector` names, or else the removed space of
    /// that ID whose files are kept.
    pub async fn file_space(&self, selector: &SpaceSelector) -> Result<Space> {
        let error = match self.space(selector).await {
            Ok(space) => return Ok(space.into()),
            Err(error) => error,
        };
        let Some(id) = selector.given_id().cloned() else {
            return Err(error);
        };
        let gone = self
            .store
            .run(move |db| records::gone_space(db, &id))
            .await?;
        gone.ok_or(error)
    }

    /// Copies of the files `results` that `space` hands back with its
    /// completion, recorded as its results attached by `attacher`.
    pub async fn keep_results(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        results: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<HandedBack>> {
        let kept = |files: Vec<SentFile>| std::future::ready(Ok(files));
        let records = (shoal_coordination::alerts::send_with_files(
            daemon,
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

    /// Copies of `files` that `space` sends with `message`, recorded as
    /// coming from `source` and attached by `attacher`, which outlive its
    /// removal, until the message is sent or refused: a refused message's
    /// copies and records go, while those an identical earlier message made
    /// stay with it.
    pub async fn attach_files(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        source: FileSource,
        message: &str,
        files: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Attaching> {
        let (layout, guard) = self.lock_files(daemon, space).await?;
        let kept = copy_for_user(layout, &space.id, message, files).await?;
        let records = self
            .record_or_discard(&space.id, source, attacher, kept.clone())
            .await?;
        Ok(Attaching {
            files: records.iter().map(SentFile::from).collect(),
            unsent: Box::new(UnsentCopies {
                store: self.store.clone(),
                kept,
                records,
                _files: guard,
            }),
        })
    }

    /// Attach copies of the files attached to the space `from` to the space
    /// `to`, each with its title and as whoever attached it, and return each
    /// one's record ID with its copy's record. The originals stay with
    /// `from`, so what refers to them keeps them for as long as it does.
    pub async fn copy_attached_files(
        &self,
        daemon: &dyn Daemon,
        from: &SpaceId,
        to: &SpaceId,
    ) -> Result<Vec<(i64, AttachedFile)>> {
        let (from, to) = (SpaceSelector::id(from), SpaceSelector::id(to));
        let mut copied = Vec::new();
        for file in self.attached_files(&from).await? {
            // Every record of this machine's store names its copy here.
            let Some(path) = file.copy.path() else {
                continue;
            };
            let new = NewFile {
                path: path.to_path_buf(),
                title: file.title.clone(),
            };
            let added = self.add_attached_files(daemon, &to, vec![new], &file.attached_by);
            copied.extend(added.await?.into_iter().map(|added| (file.id, added)));
        }
        Ok(copied)
    }

    /// Delete the files attached to the workspace or chat `selector` names,
    /// only the copies of files with one of `names` and the records `ids`
    /// when there are any, and return their records. A name or record it does
    /// not have deletes nothing.
    pub async fn remove_attached_files(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        names: Vec<String>,
        ids: Vec<i64>,
    ) -> Result<Vec<AttachedFile>> {
        let space = self.file_space(selector).await?;
        let (home, _files) = self
            .lock_files_home(daemon, &self.space_home(daemon, &space).await?)
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
    pub async fn remove_idle_files(&self, daemon: &dyn Daemon) -> Result<Vec<AttachedFile>> {
        let mut deleted = Vec::new();
        for key in self.files_homes(daemon).await? {
            // Execution admission comes first, as nothing holding the files
            // gate waits for it.
            let _admission = daemon.lock_connections().await;
            let (home, _files) = self.lock_files_home(daemon, &key).await?;
            let files = self.store.run(move |db| records::idle(db, &key)).await?;
            deleted.extend(self.delete_files(&home, files).await?);
        }
        Ok(deleted)
    }

    /// The homes of attached files: every repository's directory, and each
    /// `root_dir` that keeps files of chats without a repository.
    pub async fn files_homes(&self, daemon: &dyn Daemon) -> Result<Vec<HomeKey>> {
        let repositories = daemon.repositories().await?.into_iter();
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
        let layout = home.layout();
        let paths: Vec<PathBuf> = (files.iter())
            .filter_map(|file| file.copy.path().map(Path::to_path_buf))
            .collect();
        tokio::task::spawn_blocking(move || {
            paths.iter().try_for_each(|path| layout.delete_copy(path))
        })
        .await??;
        let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
        self.store
            .write(move |tx| records::forget(tx, &ids))
            .await?;
        Ok(files)
    }

    /// Attach copies of `files` to the workspace or running chat `selector`
    /// names, without a message, as `attacher`, and return their records.
    /// Adding the same files again keeps the copies they have, with any new
    /// title.
    pub async fn add_attached_files(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        files: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Vec<AttachedFile>> {
        ensure!(!files.is_empty(), "give the files to add");
        let space: Space = self.space(selector).await?.into();
        let (layout, _files) = self.lock_files(daemon, &space).await?;
        let kept = keep(layout, &space.id, ADDED, files).await?;
        self.record_or_discard(&space.id, FileSource::Added, attacher, kept)
            .await
    }

    /// Attach copies of the files of an agent's session to `space` for a
    /// handoff, each file with the [`SessionCopy`] it copies, whatever their
    /// size, and return their records. Files unchanged since an earlier
    /// handoff copied them keep the copies they have.
    pub async fn attach_session_files(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        files: Vec<(NewFile, SessionCopy)>,
    ) -> Result<Vec<AttachedFile>> {
        let (layout, _files) = self.lock_files(daemon, space).await?;
        let id = space.id.clone();
        let kept = tokio::task::spawn_blocking(move || layout.keep_session(&id, HANDOFF, &files))
            .await??;
        let source = FileSource::Handoff;
        (self.record_or_discard(&space.id, source, &Attacher::User, kept)).await
    }

    /// Record the copies `kept` as attached to the workspace or chat
    /// `space_id` from `source` by `attacher` and return their records, or
    /// delete the copies made for them when that fails.
    async fn record_or_discard(
        &self,
        space_id: &SpaceId,
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
    /// `space_id` from `source` by `attacher`, and return their records,
    /// unless the titles they take would leave unread alerts that hold them
    /// more than their agents receive at once.
    async fn record_kept(
        &self,
        space_id: &SpaceId,
        source: FileSource,
        attacher: &Attacher,
        kept: &Kept,
    ) -> Result<Vec<AttachedFile>> {
        let (id, attacher) = (space_id.to_owned(), attacher.clone());
        let (copies, others) = (kept.copies.clone(), self.records.clone());
        self.store
            .write(move |tx| {
                let records = records::record_all(tx, &id, source, &attacher, &copies)?;
                let paths: Vec<_> = copies.iter().map(|copy| copy.path.clone()).collect();
                others.ensure_holders_fit(tx, &paths)?;
                Ok(records)
            })
            .await
    }

    /// Where the chats without a repository keep their attached files in
    /// `root_dir`, marked as this daemon's.
    pub fn outside_files_layout(&self, root_dir: &std::path::Path) -> Layout {
        Layout::outside_repositories(root_dir, &self.root_dir_owner())
    }

    /// The home that keeps the files of `space`, as [`Self::home_of`] says.
    pub async fn space_home(&self, daemon: &dyn Daemon, space: &Space) -> Result<HomeKey> {
        (self.home_of(
            daemon,
            space.repository_id.as_ref(),
            space.root_dir.as_deref(),
        ))
        .await
    }

    /// The home that keeps the files of a space of the repository
    /// `repository`: its directory, or for a chat without one the
    /// `root_dir` it started in, or else the configured one.
    pub async fn home_of(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&RepositoryId>,
        root_dir: Option<&std::path::Path>,
    ) -> Result<HomeKey> {
        Ok(match (repository, root_dir) {
            (Some(id), _) => HomeKey::Repository(
                daemon
                    .repository(crate::repository::Selector::from(id))
                    .await?,
            ),
            (None, Some(root_dir)) => HomeKey::Root(root_dir.to_owned()),
            (None, None) => HomeKey::Root(self.config.current().root_dir(&self.paths)?),
        })
    }

    /// Hold the gate of the attached files `key` keeps, with where they are
    /// once held: a move of a repository's directory, which holds the gate,
    /// may have finished meanwhile.
    pub async fn lock_files_home(
        &self,
        daemon: &dyn Daemon,
        key: &HomeKey,
    ) -> Result<(FilesHome, OwnedMutexGuard<()>)> {
        let guard = match key {
            HomeKey::Repository(repo) => self.gates.lock_message_files(&repo.id).await,
            HomeKey::Root(_) => self.gates.lock_outside_message_files().await,
        };
        let home = match key {
            HomeKey::Repository(repo) => FilesHome::Repository(
                daemon
                    .repository(crate::repository::Selector::from(&repo.id))
                    .await?,
            ),
            HomeKey::Root(root_dir) => FilesHome::Outside(self.outside_files_layout(root_dir)),
        };
        Ok((home, guard))
    }

    /// Hold the gate of the files attached to the workspace or chat `space`,
    /// with where they are kept, once the workspace is still there and not
    /// being removed, or the chat still runs.
    pub async fn lock_files(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<(Layout, OwnedMutexGuard<()>)> {
        let (home, guard) = self
            .lock_files_home(daemon, &self.space_home(daemon, space).await?)
            .await?;
        // A move, removal or end may have finished while this waited.
        self.ensure_attachable(&space.id).await?;
        Ok((home.layout(), guard))
    }

    /// Refuse attaching files to the space `space_id` once the
    /// workspace is being removed or is gone, or the chat was removed.
    async fn ensure_attachable(&self, space_id: &SpaceId) -> Result<()> {
        match self.workspace_by_id(space_id).await {
            Ok(workspace) => ensure!(
                workspace.state != WorkspaceState::Removing,
                "the workspace is being removed"
            ),
            Err(error) if self.chat_by_id(space_id).await?.is_none() => return Err(error),
            Err(_) => {}
        }
        Ok(())
    }
}

/// The copies made for a message until it is sent, which go with their
/// records if it is refused, while the space's files stay locked.
struct UnsentCopies {
    store: crate::store::Store,
    kept: Kept,
    records: Vec<AttachedFile>,
    _files: OwnedMutexGuard<()>,
}

impl Unsent for UnsentCopies {
    /// Delete the copies and their records once the message they were made
    /// for was refused, unless the copies were reused. Failure only logs,
    /// as the message's refusal is what its sender hears.
    fn forget(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            if self.kept.reused() {
                return;
            }
            let ids: Vec<i64> = self.records.iter().map(|file| file.id).collect();
            if let Err(error) = self.store.write(move |tx| records::forget(tx, &ids)).await {
                crate::log!("records of refused files not deleted: {error:#}");
            }
            discard(self.kept).await;
        })
    }
}

#[cfg(test)]
mod tests;
