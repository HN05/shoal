//! Chats: agents the user talks with in a repository's registered checkout,
//! or in a workspace's worktree, where they are not the workspace's agent,
//! or without a repository in `root_dir`, managing the spaces of every
//! repository.
//! They are records, not tracked executions, so they never queue or stop
//! under pressure. A chat runs while its process lives; once its agent exits
//! or Shoal saves it, the chat is stopped until it is restored, and it lasts
//! until it is removed.
use crate::client::agent::AgentSessionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::{SpaceId, SpaceNumber};
use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        approvals::{ApprovalRequest, release_decided_by},
        log, store,
        workspace::{self, Manager},
    },
    fsutil::off_runtime,
    model::{
        Chat, RenamedChat, Repository, SpaceKind, SpaceLinks, SpaceRef, Workspace, WorkspaceRef,
    },
    process::identity::{self, Identity},
    state::ChatRestore,
    tmp_dirs,
};

/// How soon a chat's stop reaches the journal when nothing lists chats, and
/// a saved chat comes back once agents may start.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = manager.stop_finished_chats().await {
            log!("chats: {error:#}");
        }
        if let Err(error) = restores::restore_automatic(&manager).await.map(drop) {
            log!("chats: {error:#}");
        }
    }
}

impl Manager {
    /// Record the chat `name` whose agent replaces process `pid` in the
    /// repository's checkout, or when `hands_over`, runs in a session `pid`
    /// starts and [`Self::move_chat`] names, at the top.
    #[cfg(test)]
    pub async fn start_chat(
        &self,
        repository: crate::repository::Selector,
        name: SpaceName,
        agent: crate::agent::Agent,
        pid: u32,
        origin: Option<crate::sessions::Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let new = NewChat {
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            pid,
            origin,
            hands_over,
            parent: None,
        };
        self.start_new_chat(Some(repository), None, new).await
    }

    /// [`Self::start_chat`] in the worktree of the workspace `workspace`
    /// names, which must be ready, in the workspace's repository.
    #[cfg(test)]
    pub async fn start_chat_in_workspace(
        &self,
        workspace: &SpaceSelector,
        name: SpaceName,
        agent: crate::agent::Agent,
        pid: u32,
        origin: Option<crate::sessions::Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let workspace = self.chat_workspace(workspace).await?;
        let new = NewChat {
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            pid,
            origin,
            hands_over,
            parent: None,
        };
        let repository = Some(workspace.repository_id.clone());
        self.record_new_chat(repository, Some(workspace_ref(&workspace)), new)
            .await
    }

    /// Record the chat `new` in the checkout of `repository`, in the
    /// worktree of the workspace `workspace` names, which must be ready, in
    /// the workspace's repository, or without either in `root_dir`.
    pub async fn start_new_chat(
        &self,
        repository: Option<crate::repository::Selector>,
        workspace: Option<&SpaceSelector>,
        new: NewChat,
    ) -> Result<Chat> {
        let (repository, workspace) = match (workspace, repository) {
            (Some(selector), _) => {
                let workspace = self.chat_workspace(selector).await?;
                (
                    Some(workspace.repository_id.clone()),
                    Some(workspace_ref(&workspace)),
                )
            }
            (None, Some(repository)) => (Some(self.repository(repository).await?.id), None),
            (None, None) => (None, None),
        };
        self.record_new_chat(repository, workspace, new).await
    }

    /// Refuse a named `parent` that is `workspace`, whose worktree the chat
    /// runs in: the chat is not the workspace's child, whose questions would
    /// go to the workspace's agent while the user attends the chat.
    pub(crate) async fn ensure_parent_not_host(
        &self,
        workspace: Option<&SpaceSelector>,
        parent: Option<&SpaceSelector>,
    ) -> Result<()> {
        let (Some(workspace), Some(parent)) = (workspace, parent) else {
            return Ok(());
        };
        let host = self.workspace(workspace).await?;
        let parent = self.space(parent).await?;
        ensure!(
            *parent.id() != host.id,
            "a chat in the worktree of {} cannot go below it; name another --parent",
            host.name
        );
        Ok(())
    }

    /// The workspace `selector` names, once a chat may run in it: it is
    /// ready and its worktree is there and verified to be its own.
    pub(crate) async fn chat_workspace(&self, selector: &SpaceSelector) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        ensure!(
            workspace.state == crate::state::WorkspaceState::Ready,
            "workspace {} is {}, and a chat runs only in a ready workspace",
            workspace.name,
            workspace.state
        );
        let path = &workspace.path;
        ensure!(
            self.check_access(path, off_runtime::is_dir(path)).await?,
            "the worktree of workspace {} is missing at {}",
            workspace.name,
            path.display()
        );
        self.check_access(path, self.verify_worktree(&workspace))
            .await?;
        Ok(workspace)
    }

    /// Record `new` as a chat of the repository `repository`, or of none,
    /// in the worktree of `workspace` when given, below its parent, which
    /// must take a chat of that repository, or at the top without one.
    async fn record_new_chat(
        &self,
        repository: Option<RepositoryId>,
        workspace: Option<WorkspaceRef>,
        new: NewChat,
    ) -> Result<Chat> {
        let NewChat {
            name,
            agent,
            mode,
            narrowing,
            pid,
            origin,
            hands_over,
            parent,
        } = new;
        let home = self.reserve_chat_home(repository).await?;
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        // A save under way would miss it, and a shutdown's would not come again.
        let _saving = self.chat_saves.lock().await;
        ensure!(
            !self.agent_queue.suspended(),
            "Shoal is stopping work for a shutdown or a daemon stop; start the chat once it is over"
        );
        let started_at = crate::time::UnixTime::now();
        let chat = Chat {
            id: SpaceId::generate(),
            number: SpaceNumber::default(),
            repository_id: (home.repository.as_ref()).map(|repository| repository.id.clone()),
            name,
            agent,
            mode,
            narrowing,
            workspace,
            pid: process.pid,
            started_at,
            origin,
            tmp_dir: None,
            root_dir: home.root_dir.clone(),
            session: None,
            restore: None,
            links: SpaceLinks::default(),
            review: Vec::new(),
            completion: None,
            agent_state: None,
            progress: None,
            parent: None,
            children: Vec::new(),
        };
        let (recorded, removing) = (process.clone(), self.chat_removals.clone());
        let below = parent.map(|parent| (parent, removing));
        let chat = self
            .store
            .write(move |tx| insert(tx, chat, &recorded, hands_over, below))
            .await?;
        let chat = (self.give_tmp_dir(chat, home.repository.as_ref(), process)).await?;
        self.take_chat_place(&chat).await?;
        Ok(chat)
    }

    /// Where a new chat of `repository` keeps its temporary directory: the
    /// repository's directory, reserved now, or for a chat without one the
    /// configured `root_dir`, which stays its own however `root_dir` changes
    /// later. A chat without one is recorded under the registry gate, so a
    /// root move, which holds it, finds the chat before clearing `root_dir`.
    async fn reserve_chat_home(&self, repository: Option<RepositoryId>) -> Result<ChatHome<'_>> {
        Ok(match repository {
            Some(id) => ChatHome {
                repository: Some(self.reserve_repository_directory(&id).await?),
                root_dir: None,
                _registry: None,
            },
            None => ChatHome {
                repository: None,
                _registry: Some(self.gates.lock_registry().await),
                root_dir: Some(self.config().root_dir(&self.paths)?),
            },
        })
    }

    /// `chat`, just recorded with `process`, with its temporary directory,
    /// created now; a chat that cannot have one is forgotten, having never
    /// run.
    async fn give_tmp_dir(
        &self,
        mut chat: Chat,
        repository: Option<&Repository>,
        process: Identity,
    ) -> Result<Chat> {
        match self.chat_tmp_dir(repository, &chat) {
            Ok(tmp_dir) => {
                chat.tmp_dir = tmp_dir;
                Ok(chat)
            }
            Err(error) => {
                let id = chat.id.clone();
                self.store
                    .run(move |db| forget_unstarted(db, &id, &process))
                    .await?;
                Err(error)
            }
        }
    }

    /// Refuse a name a new chat could not take now.
    pub async fn check_chat_name(&self, name: SpaceName) -> Result<()> {
        self.store
            .run(move |db| store::ensure_name_available(db, &name, None))
            .await
    }

    /// `name`, made a valid name, or its lowest `-2`, `-3`, ... variant
    /// that a new chat could take now.
    pub async fn free_chat_name(&self, name: String) -> Result<SpaceName> {
        let base = SpaceName::derive(SpaceKind::Chat, &name);
        self.store.run(move |db| store::free_name(db, &base)).await
    }

    /// Give the chat `chat` the name `name`, which must be free as a new
    /// chat's, and move its files to that name.
    pub async fn rename_chat(&self, chat: SpaceId, name: SpaceName) -> Result<RenamedChat> {
        let (paths, owner) = (self.paths.clone(), self.root_dir_owner());
        // The store's one thread moves the files after the rename commits,
        // so no other rename or chat start reaches either name in between.
        self.store
            .run(move |db| {
                let (old, renamed) =
                    store::write_transaction(db, |tx| rename(tx, &owner, &chat, &name))?;
                // The chat has its new name even when its files stay behind.
                if old.name != renamed.name
                    && let Err(error) = move_chat_files(&paths, &old, &renamed)
                {
                    log!("chat {}: {error:#}", renamed.id);
                }
                Ok(RenamedChat {
                    chat: renamed,
                    previous_name: old.name,
                })
            })
            .await
    }

    /// Make process `pid` the one whose life a running chat lasts.
    pub async fn move_chat(&self, chat: &SpaceId, pid: u32) -> Result<Chat> {
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let chat = chat.to_owned();
        let _handoff = self.chat_handoffs.write().await;
        let owner = self.root_dir_owner();
        self.store
            .run(move |db| move_to(db, &owner, &chat, &process))
            .await
    }

    /// Record `session` as the agent session the running chat `id` continues.
    pub(crate) async fn record_chat_session(
        &self,
        id: &SpaceId,
        session: AgentSessionId,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE chats SET session=?2 WHERE id=?1",
                    params![id, session],
                )?;
                Ok(())
            })
            .await
    }

    /// Running and stopped chats in start order, after stopping those whose
    /// process exited.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        self.stop_finished_chats().await?;
        let owner = self.root_dir_owner();
        self.store.run(move |db| recorded(db, &owner)).await
    }

    /// The agent processes of the running chats.
    pub(crate) async fn chat_processes(&self) -> Result<Vec<Identity>> {
        let recorded = self.store.run(|db| recorded_processes(db)).await?;
        Ok(recorded.into_iter().map(|(_, process)| process).collect())
    }

    /// The running chat with the ID `id`.
    pub(crate) async fn running_chat_by_id(&self, id: &SpaceId) -> Result<Option<Chat>> {
        Ok((self.chat_by_id(id).await?).filter(|chat| chat.restore.is_none()))
    }

    /// The running or stopped chat with the ID `id`.
    pub(crate) async fn chat_by_id(&self, id: &SpaceId) -> Result<Option<Chat>> {
        Ok(self.chats().await?.into_iter().find(|chat| chat.id == *id))
    }

    /// Stop the running chat `id`: stop every process it owns, so none goes
    /// on writing the agent's session, then record the chat stopped, as when
    /// the process it runs as exits. Until the stop finishes, the chat stays
    /// running, so nothing restores it meanwhile.
    pub(crate) async fn stop_chat(&self, id: &SpaceId) -> Result<()> {
        self.stop_chat_recording(id, |found| self.record_chat_stopping(id, found))
            .await
    }

    /// [`Self::stop_chat`], with `record` keeping what each round of the
    /// stop signals before it does.
    async fn stop_chat_recording<R>(
        &self,
        id: &SpaceId,
        record: impl Fn(Vec<Identity>) -> R,
    ) -> Result<()>
    where
        R: Future<Output = Result<()>>,
    {
        let stopping = self.chat_stops.hold(id);
        let chat = id.to_owned();
        let recorded = self
            .store
            .run(move |db| recorded_process(db, &chat))
            .await?
            .context("the chat is not running")?;
        let wrappers = self.execution_wrappers().await?;
        let stopped = self.stopped_chat_processes(id).await?;
        processes::ChatProcesses::of(id, &recorded, wrappers)?
            .context("the chat is not running")?
            .stop(stopped, record)
            .await?;
        drop(stopping);
        self.stop_finished_chats().await
    }

    /// The wrappers of the executions running, which belong to their
    /// workspaces even when a chat started them.
    async fn execution_wrappers(&self) -> Result<Vec<Identity>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        Ok(executions
            .into_iter()
            .filter_map(|execution| execution.wrapper)
            .collect())
    }

    /// Whether Shoal saved the stopped chat `id`'s session, rather than its
    /// agent exiting.
    pub(crate) async fn chat_saved(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM chats WHERE id=?1 AND restore IS NOT NULL AND saved",
                    [id],
                )
            })
            .await
    }

    /// The chats running or stopped in the worktree of the workspace `id`,
    /// by name.
    pub(crate) async fn chats_in(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare("SELECT id,name FROM chats WHERE workspace_id=?1 ORDER BY name")?
                    .query_map([id], |row| {
                        Ok(SpaceRef {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            kind: SpaceKind::Chat,
                            branch: None,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The workspace in whose worktree the space `id` runs, when it is a
    /// chat there.
    pub(crate) async fn chat_home(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(
                    (db.query_row("SELECT workspace_id FROM chats WHERE id=?1", [id], |row| {
                        row.get(0)
                    }))
                    .optional()?
                    .flatten(),
                )
            })
            .await
    }

    /// Whether a chat runs in the worktree of the workspace `id`.
    pub(crate) async fn chat_runs_in(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM chats WHERE workspace_id=?1 AND restore IS NULL",
                    [id],
                )
            })
            .await
    }

    /// Whether Shoal saved the session of a stopped chat in the worktree of
    /// the workspace `id`.
    pub(crate) async fn chat_saved_in(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM chats WHERE workspace_id=?1 AND restore IS NOT NULL AND saved",
                    [id],
                )
            })
            .await
    }

    /// When the chat `id`'s agent stopped, while it is stopped.
    pub(crate) async fn chat_stopped_at(
        &self,
        id: &SpaceId,
    ) -> Result<Option<crate::time::UnixTime>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let stopped_at = db
                    .query_row("SELECT stopped_at FROM chats WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                Ok(stopped_at.flatten())
            })
            .await
    }

    /// Stop the running chats whose process has exited, other than those a
    /// stop is under way for, which stop once it finishes.
    pub(crate) async fn stop_finished_chats(&self) -> Result<()> {
        let exited: Vec<_> = finished(self.store.run(|db| recorded_processes(db)).await?)
            .into_iter()
            // Read after the processes: a stop's hold begins before it
            // signals the agent.
            .filter(|(id, _)| !self.chat_stops.under_way(id))
            .collect();
        if exited.is_empty() {
            return Ok(());
        }
        let released = self.store.write(move |tx| stop(tx, &exited)).await?;
        self.ask_user_about(&released).await;
        Ok(())
    }

    /// The settings of `chat`: its repository's, or the global config's
    /// alone for a chat without a repository.
    pub(crate) async fn chat_settings(&self, chat: &Chat) -> Result<crate::config::Effective> {
        match &chat.repository_id {
            Some(id) => self.repository_settings(id).await,
            None => self.config().resolve(&Default::default()),
        }
    }

    /// The registered checkout of `chat`'s repository; none for a chat
    /// without a repository.
    pub(crate) async fn chat_repository(&self, chat: &Chat) -> Result<Option<Repository>> {
        match &chat.repository_id {
            Some(id) => Ok(Some(self.repository(id).await?)),
            None => Ok(None),
        }
    }

    /// The directory a chat outside a workspace runs in: its repository's
    /// registered checkout, or the `root_dir` a chat without a repository
    /// started in.
    pub(crate) async fn chat_checkout_path(&self, chat: &Chat) -> Result<std::path::PathBuf> {
        match (self.chat_repository(chat).await?, &chat.root_dir) {
            (Some(repository), _) => Ok(repository.path),
            (None, Some(root_dir)) => Ok(root_dir.clone()),
            (None, None) => self.config().root_dir(&self.paths),
        }
    }

    /// The layout that keeps the temporary directory of `chat` of
    /// `repository`, or for a chat without one of the `root_dir` it started
    /// in; none in a repository without a directory.
    pub(crate) fn chat_tmp_layout(
        &self,
        repository: Option<&Repository>,
        chat: &Chat,
    ) -> Option<tmp_dirs::Layout> {
        match repository {
            Some(repository) => tmp_dirs::Layout::of(repository),
            None => (chat.root_dir.as_deref())
                .map(|root| tmp_dirs::Layout::outside_repositories(root, &self.root_dir_owner())),
        }
    }

    /// The layout of the temporary directories of the chats without a
    /// repository in the configured `root_dir`, which no other daemon uses or
    /// sweeps; none while `root_dir` is not valid, when no such chat starts
    /// either.
    pub(crate) fn outside_tmp_layout(&self) -> Option<tmp_dirs::Layout> {
        let root_dir = self.config().root_dir(&self.paths).ok()?;
        Some(tmp_dirs::Layout::outside_repositories(
            &root_dir,
            &self.root_dir_owner(),
        ))
    }

    /// The state directory as the daemon marks what it keeps in `root_dir`:
    /// canonical, so every path to it names the same owner.
    pub(crate) fn root_dir_owner(&self) -> std::path::PathBuf {
        std::fs::canonicalize(&self.paths.state).unwrap_or_else(|_| self.paths.state.clone())
    }

    /// The temporary directory of `chat` of `repository`, or of none,
    /// created when missing.
    pub(crate) fn chat_tmp_dir(
        &self,
        repository: Option<&Repository>,
        chat: &Chat,
    ) -> Result<Option<std::path::PathBuf>> {
        (self.chat_tmp_layout(repository, chat))
            .map(|layout| layout.prepare(&chat.id))
            .transpose()
    }

    /// The scope token the agent of the chat `id` carries.
    pub(crate) async fn chat_scope(&self, id: &SpaceId) -> Result<ScopeToken> {
        let id = id.to_owned();
        let token = self.store.run(move |db| scope_token(db, &id)).await?;
        token.context("no such chat")
    }
}

/// What a process records to start a chat as itself: the chat's name,
/// agent and mode, where it runs in a terminal host, whether it `hands_over` to a
/// session it starts, and the space it goes below.
pub struct NewChat {
    pub name: SpaceName,
    pub agent: crate::agent::Agent,
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub pid: u32,
    pub origin: Option<crate::sessions::Origin>,
    pub hands_over: bool,
    pub parent: Option<SpaceId>,
}

/// The reference a chat's record gives to the workspace it runs in.
fn workspace_ref(workspace: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

/// Insert `chat` unless its name is taken, or the repository or the
/// workspace it runs in is being removed, starting until it is moved when
/// it `hands_over`, below the space `parent` when given, which must take it
/// and, as `removing` shows, not be a chat being removed.
/// Where a new chat keeps its temporary directory, and the registry gate a
/// chat without a repository is recorded under.
struct ChatHome<'a> {
    repository: Option<Repository>,
    root_dir: Option<std::path::PathBuf>,
    _registry: Option<tokio::sync::MutexGuard<'a, ()>>,
}

fn insert(
    tx: &rusqlite::Transaction<'_>,
    mut chat: Chat,
    process: &Identity,
    hands_over: bool,
    parent: Option<(SpaceId, Removing)>,
) -> Result<Chat> {
    let workspace = chat.workspace.as_ref().map(|workspace| &workspace.id);
    ensure_place_free(tx, chat.repository_id.as_ref(), workspace)?;
    store::ensure_name_available(tx, &chat.name, None)?;
    tx.execute(
        "INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at,origin,starting,
            unique_id,scope_token,workspace_id,mode,root_dir,narrowing)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            chat.id,
            chat.repository_id,
            chat.name,
            chat.agent,
            chat.pid,
            process.birth,
            chat.started_at,
            chat.origin
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            hands_over,
            unique_id(process)?,
            ScopeToken::generate(),
            workspace,
            chat.mode,
            (chat.root_dir.as_deref()).map(|root| root.to_string_lossy().into_owned()),
            store::narrowing_column(&chat.narrowing)?
        ],
    )?;
    chat.number = tx.query_row("SELECT number FROM chats WHERE id=?1", [&chat.id], |row| {
        row.get(0)
    })?;
    // Checked with the record, so a removal that began while the start
    // waited finds the chat below the parent or refuses it.
    if let Some((parent, removing)) = &parent {
        ensure!(!removing.reserved(parent), "{parent} is being removed");
        workspace::ensure_parent(tx, chat.repository_id.as_ref(), &chat.id, parent)?;
        workspace::set_parent(tx, &chat.id, Some(parent))?;
        chat.parent = workspace::parent(tx, &chat.id)?;
    }
    Ok(chat)
}

/// The scope token of the chat `id`, which its agent carries.
pub(crate) fn scope_token(db: &Connection, id: &SpaceId) -> Result<Option<ScopeToken>> {
    Ok(db
        .query_row("SELECT scope_token FROM chats WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten())
}

/// The never-reused ID of `process` as JSON, where the platform reports one,
/// which finds what the chat's agent started once it ended, as after it
/// exits.
pub(super) fn unique_id(process: &Identity) -> Result<Option<String>> {
    Ok(process
        .unique
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?)
}

/// Refuse a chat in the repository `repository_id` while it is being
/// removed or moved, or in the workspace `workspace` unless it is ready, as
/// it is not while it is being removed.
fn ensure_place_free(
    tx: &rusqlite::Transaction<'_>,
    repository_id: Option<&RepositoryId>,
    workspace: Option<&SpaceId>,
) -> Result<()> {
    if let Some(repository_id) = repository_id {
        ensure!(
            !store::repository_removing(tx, repository_id)?,
            "the repository is being removed"
        );
        // A chat edits the checkout, which a move may be copying.
        store::ensure_repository_not_moving(tx, repository_id)?;
    }
    let Some(workspace) = workspace else {
        return Ok(());
    };
    let state: Option<crate::state::WorkspaceState> = tx
        .query_row(
            "SELECT state FROM workspaces WHERE id=?1",
            [workspace],
            |row| row.get(0),
        )
        .optional()?;
    match state {
        Some(crate::state::WorkspaceState::Ready) => Ok(()),
        Some(state) => {
            bail!("the chat's workspace is {state}, and a chat runs only in a ready one")
        }
        None => bail!("the chat's workspace was removed"),
    }
}

/// Rename the running or stopped chat `chat`, by ID or name, to `name` unless
/// another holds it. Returns the chat before and after.
fn rename(
    tx: &rusqlite::Transaction<'_>,
    owner: &Path,
    chat: &SpaceId,
    name: &SpaceName,
) -> Result<(Chat, Chat)> {
    let old = recorded(tx, owner)?
        .into_iter()
        .find(|running| running.id == *chat)
        .with_context(|| format!("no running chat {chat}"))?;
    if old.name != *name {
        // The starting chat's files are not open yet under its name.
        ensure!(
            !store::exists(
                tx,
                "SELECT 1 FROM chats WHERE id=?1 AND starting",
                [&old.id]
            )?,
            "chat {} is still starting; rename it once it runs",
            old.name
        );
        store::ensure_name_available(tx, name, None)?;
        tx.execute(
            "UPDATE chats SET name=?2 WHERE id=?1",
            params![old.id, name],
        )?;
    }
    let renamed = Chat {
        name: name.clone(),
        ..old.clone()
    };
    Ok((old, renamed))
}

/// Move the files kept under `old`'s name to `renamed`'s, replacing those
/// a removed chat of that name left. A running session keeps writing to the
/// log it holds open.
fn move_chat_files(paths: &crate::paths::Paths, old: &Chat, renamed: &Chat) -> Result<()> {
    let (from, to) = (paths.chat_files(old), paths.chat_files(renamed));
    for extension in [crate::paths::SESSION_LOG, crate::paths::SESSION_PROMPT] {
        let (from, to) = (from.with_extension(extension), to.with_extension(extension));
        match std::fs::rename(&from, &to) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            moved => moved.with_context(|| format!("cannot move {}", from.display()))?,
        }
    }
    Ok(())
}

/// Make `process` the one the running chat `chat` runs as. A stopped
/// chat's agent was stopped, so a session started for it is refused, which
/// its launcher then stops.
fn move_to(db: &mut Connection, owner: &Path, chat: &SpaceId, process: &Identity) -> Result<Chat> {
    let moved = db.execute(
        "UPDATE chats SET pid=?2,birth=?3,unique_id=?4,starting=0 WHERE id=?1 AND restore IS NULL",
        params![chat, process.pid, process.birth, unique_id(process)?],
    )?;
    anyhow::ensure!(moved == 1, "no running chat {chat}");
    recorded(db, owner)?
        .into_iter()
        .find(|running| running.id == *chat)
        .context("the chat ended")
}

/// The running and stopped chats in start order, a chat without a
/// repository with its temporary directory in the `root_dir` it started in,
/// marked as `owner`'s, the daemon's state directory.
fn recorded(db: &Connection, owner: &Path) -> Result<Vec<Chat>> {
    db.prepare(
        "SELECT chats.id,chats.repository_id,chats.name,chats.agent,chats.pid,
                    chats.started_at,chats.origin,repositories.workspaces_dir,chats.session,
                    chats.restore,chats.completion,
                    workspaces.id,workspaces.name,workspaces.branch,chats.number,chats.mode,
                    chats.root_dir,chats.narrowing
             FROM chats LEFT JOIN repositories ON repositories.id=chats.repository_id
                LEFT JOIN workspaces ON workspaces.id=chats.workspace_id
             ORDER BY chats.started_at, chats.rowid",
    )?
    .query_map([], |row| {
        let id: SpaceId = row.get(0)?;
        let repository_id: Option<RepositoryId> = row.get(1)?;
        let root_dir = row
            .get::<_, Option<String>>(16)?
            .map(std::path::PathBuf::from);
        let tmp_dir = match (&repository_id, row.get::<_, Option<String>>(7)?) {
            (Some(repository_id), Some(directory)) => {
                Some(tmp_dirs::Layout::new(Path::new(&directory), repository_id).locate(&id))
            }
            (Some(_), None) => None,
            (None, _) => (root_dir.as_deref())
                .map(|root| tmp_dirs::Layout::outside_repositories(root, owner).locate(&id)),
        };
        Ok(Chat {
            id,
            number: row.get(14)?,
            repository_id,
            name: row.get(2)?,
            agent: row.get(3)?,
            mode: row.get(15)?,
            narrowing: store::json_column(row, "narrowing")?.unwrap_or_default(),
            workspace: match row.get::<_, Option<SpaceId>>(11)? {
                Some(id) => Some(WorkspaceRef {
                    id,
                    name: row.get(12)?,
                    branch: row.get(13)?,
                }),
                None => None,
            },
            pid: row.get(4)?,
            started_at: row.get(5)?,
            origin: store::json_column(row, "origin")?,
            tmp_dir,
            root_dir,
            session: row.get(8)?,
            restore: row.get(9)?,
            links: SpaceLinks::default(),
            review: Vec::new(),
            completion: store::json_column(row, "completion")?,
            agent_state: None,
            progress: None,
            parent: None,
            children: Vec::new(),
        })
    })?
    .collect::<rusqlite::Result<Vec<Chat>>>()?
    .into_iter()
    .map(|mut chat| {
        chat.links = store::space_links(db, &chat.id)?;
        chat.review = crate::daemon::workspace::review::list(db, &chat.id)?;
        chat.parent = workspace::parent(db, &chat.id)?;
        chat.children = workspace::children(db, &chat.id)?;
        chat.agent_state = workspace::agent_state::current(db, &chat.id)?;
        chat.progress = crate::daemon::progress::current(db, &chat.id)?;
        Ok(chat)
    })
    .collect()
}

/// The process the chat `id` runs as, unless it is stopped.
fn recorded_process(db: &mut Connection, id: &SpaceId) -> Result<Option<Identity>> {
    Ok(db
        .query_row(
            "SELECT pid,birth FROM chats WHERE id=?1 AND restore IS NULL",
            [id],
            |row| {
                Ok(Identity {
                    pid: row.get(0)?,
                    birth: row.get(1)?,
                    unique: None,
                })
            },
        )
        .optional()?)
}

/// The process each running chat runs as.
fn recorded_processes(db: &Connection) -> Result<Vec<(SpaceId, Identity)>> {
    Ok(db
        .prepare("SELECT id,pid,birth FROM chats WHERE restore IS NULL")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Identity {
                    pid: row.get(1)?,
                    birth: row.get(2)?,
                    unique: None,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The chats whose process no longer has its recorded identity. A process
/// that cannot be read keeps its chat.
fn finished(recorded: Vec<(SpaceId, Identity)>) -> Vec<(SpaceId, Identity)> {
    recorded
        .into_iter()
        .filter(|(id, process)| match identity::capture(process.pid) {
            Ok(current) => current.as_ref() != Some(process),
            Err(error) => {
                log!("chat {id}: {error:#}");
                false
            }
        })
        .collect()
}

/// Record the chats still running as the processes `exited` found ended
/// stopped, to wait for `shoal resume`, returning the requests they decided,
/// given back to the user, since a stopped chat decides none.
fn stop(
    tx: &rusqlite::Transaction<'_>,
    exited: &[(SpaceId, Identity)],
) -> Result<Vec<ApprovalRequest>> {
    let mut released = Vec::new();
    for (id, process) in exited {
        let stopped = tx.execute(
            "UPDATE chats SET restore=?4,starting=0,stopped_at=unixepoch(),saved=0,stopped_by=NULL
             WHERE id=?1 AND pid=?2 AND birth=?3 AND restore IS NULL",
            params![id, process.pid, process.birth, ChatRestore::Resume],
        )?;
        if stopped == 1 {
            released.extend(release_decided_by(tx, id)?);
        }
    }
    Ok(released)
}

/// Delete the record of the chat `id`, just started as `process`, which
/// never ran.
fn forget_unstarted(db: &mut Connection, id: &SpaceId, process: &Identity) -> Result<()> {
    db.execute(
        "DELETE FROM chats WHERE id=?1 AND pid=?2 AND birth=?3 AND restore IS NULL",
        params![id, process.pid, process.birth],
    )?;
    Ok(())
}

mod agents;
mod done;
mod processes;
mod removal;
mod removing;
mod restart;
mod restores;
mod saved;
mod stopped;
mod stopping;

pub(crate) use agents::ChatAgents;
pub(crate) use removing::Removing;
pub(crate) use stopping::Stopping;

#[cfg(test)]
mod tests;
