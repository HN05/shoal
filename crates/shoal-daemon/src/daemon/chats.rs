//! Chats: agents the user talks with in a repository's registered checkout,
//! or in a workspace's worktree, where they are not the workspace's agent,
//! or without a repository in `root_dir`, managing the spaces of every
//! repository. A chat's agent runs under the execution wrapper as an
//! execution of the chat, which runs while its execution is recorded; the
//! chat lasts until it is removed.
use crate::client::agent::AgentSessionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::{SpaceId, SpaceNumber};
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events, store,
        workspace::{self, Manager},
    },
    fsutil::off_runtime,
    model::{
        Chat, RenamedChat, Repository, SpaceKind, SpaceLinks, SpaceRef, Workspace, WorkspaceRef,
    },
    tmp_dirs,
};

impl Manager {
    /// Record the chat `name` in the repository's checkout, at the top, with
    /// its agent running.
    #[cfg(test)]
    pub async fn start_chat(
        self: &std::sync::Arc<Self>,
        repository: crate::repository::Selector,
        name: SpaceName,
        agent: crate::agent::Agent,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let new = NewChat {
            priority: None,
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            origin,
            parent: None,
        };
        let chat = self.start_new_chat(Some(repository), None, new).await?;
        self.run_chat_agent(chat).await
    }

    /// [`Self::start_chat`] in the worktree of the workspace `workspace`
    /// names, which must be ready, in the workspace's repository.
    #[cfg(test)]
    pub async fn start_chat_in_workspace(
        self: &std::sync::Arc<Self>,
        workspace: &SpaceSelector,
        name: SpaceName,
        agent: crate::agent::Agent,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let workspace = self.chat_workspace(workspace).await?;
        let new = NewChat {
            priority: None,
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            origin,
            parent: None,
        };
        let repository = Some(workspace.repository_id.clone());
        let chat = (self.record_new_chat(repository, Some(workspace_ref(&workspace)), new)).await?;
        self.run_chat_agent(chat).await
    }

    /// Run the agent of `chat` as its execution, as its wrapper registers
    /// and tracks it, and return the chat running it. Asked to stop, the
    /// agent ends its processes and exits, as its wrapper stops it.
    #[cfg(test)]
    pub(crate) async fn run_chat_agent(self: &std::sync::Arc<Self>, chat: Chat) -> Result<Chat> {
        let place = (chat.origin.clone()).map(|origin| workspace::places::ExecutionPlace {
            origin,
            agent: true,
        });
        let request = workspace::ExecutionRequest {
            place,
            agent: true,
            ..Default::default()
        };
        let selector = SpaceSelector::id(&chat.id);
        let kind = workspace::ExecutionKind::Command;
        let mut started =
            (self.begin_execution_placed(&selector, None, kind, None, request)).await?;
        let agent = crate::execution::TrackedAgent {
            name: chat.agent.clone(),
            model: Default::default(),
            mode: chat.mode.clone(),
            narrowing: chat.narrowing,
        };
        let id = started.plan.id.clone();
        let running = std::time::Duration::ZERO;
        (self.track_agent(&id, &agent, &chat.id, &chat.name, true, running)).await;
        let manager = self.clone();
        tokio::spawn(async move {
            if started.stop.wait_for(|stop| *stop).await.is_ok() {
                manager.end_test_execution(id).await;
            }
        });
        self.chat_by_id(&chat.id)
            .await?
            .context("the chat was removed")
    }

    /// Record the chat `new` in the checkout of `repository`, in the
    /// worktree of the workspace `workspace` names, which must be ready, in
    /// the workspace's repository, or without either in `root_dir`.
    pub async fn start_new_chat(
        &self,
        repository: Option<crate::repository::Selector>,
        workspace: Option<&SpaceSelector>,
        mut new: NewChat,
    ) -> Result<Chat> {
        new.name = new.name.checked()?;
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
            origin,
            parent,
            priority,
        } = new;
        let home = self.reserve_chat_home(repository).await?;
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
            started_at,
            origin,
            tmp_dir: None,
            root_dir: home.root_dir.clone(),
            session: None,
            running: false,
            links: SpaceLinks::default(),
            review: Vec::new(),
            completion: None,
            agent_state: None,
            progress: None,
            sleep: None,
            asleep: None,
            parent: None,
            children: Vec::new(),
            converted: false,
        };
        let below = parent.map(|parent| (parent, self.space_removals.clone()));
        let chat = self
            .store
            .write(move |tx| {
                let chat = insert(tx, chat, below)?;
                if let Some(priority) = priority {
                    super::spaces::priority::set(tx, &chat.id, priority)?;
                }
                Ok(chat)
            })
            .await?;
        let chat = (self.give_tmp_dir(chat, home.repository.as_ref())).await?;
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

    /// `chat`, just recorded, with its temporary directory, created now; a
    /// chat that cannot have one is forgotten, having never run.
    async fn give_tmp_dir(&self, mut chat: Chat, repository: Option<&Repository>) -> Result<Chat> {
        match self.chat_tmp_dir(repository, &chat) {
            Ok(tmp_dir) => {
                chat.tmp_dir = tmp_dir;
                Ok(chat)
            }
            Err(error) => {
                let id = chat.id.clone();
                let failure = format!("{error:#}");
                (self.store)
                    .write(move |tx| forget_unstarted(tx, &id, &failure))
                    .await?;
                Err(error)
            }
        }
    }

    /// Give the chat `chat` the name `name`.
    pub async fn rename_chat(&self, chat: SpaceId, name: SpaceName) -> Result<RenamedChat> {
        // A request's name reads as sent, so the rule applies here.
        let name = SpaceName::parse(&name)?;
        let owner = self.root_dir_owner();
        let (old, renamed) = self
            .store
            .write(move |tx| rename(tx, &owner, &chat, &name))
            .await?;
        Ok(RenamedChat {
            chat: renamed,
            previous_name: old.name,
        })
    }

    /// Record `session` as the agent session the running chat `id` continues.
    pub(crate) async fn record_chat_session(
        &self,
        id: &SpaceId,
        session: AgentSessionId,
    ) -> Result<()> {
        let (chat, reported) = (id.to_owned(), session.clone());
        let recorded = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "UPDATE chats SET session=?2 WHERE id=?1",
                    params![chat, reported],
                )?)
            })
            .await?;
        // A chat that became a workspace since the report found it: its
        // agent's session goes with the conversion.
        if recorded == 0 {
            self.record_converting_session(id, session).await?;
        }
        Ok(())
    }

    /// Running and stopped chats in start order.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        let owner = self.root_dir_owner();
        self.store.run(move |db| recorded(db, &owner)).await
    }

    /// The running or stopped chat with the ID `id`.
    pub(crate) async fn chat_by_id(&self, id: &SpaceId) -> Result<Option<Chat>> {
        Ok(self.chats().await?.into_iter().find(|chat| chat.id == *id))
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

    /// Whether a chat runs its agent in the worktree of the workspace `id`.
    pub(crate) async fn chat_runs_in(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM chats JOIN executions ON executions.space_id=chats.id
                     WHERE chats.workspace_id=?1",
                    [id],
                )
            })
            .await
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
        (self.tmp_layout(repository, chat.root_dir.as_deref()))
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

/// What a process records to start a chat: the chat's name, agent and
/// mode, where it runs in a terminal host, and the space it goes below.
pub struct NewChat {
    pub name: SpaceName,
    pub agent: crate::agent::Agent,
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub origin: Option<crate::sessions::Origin>,
    pub parent: Option<SpaceId>,
    pub priority: Option<crate::client::priority::Priority>,
}

/// The reference a chat's record gives to the workspace it runs in.
fn workspace_ref(workspace: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

/// Where a new chat keeps its temporary directory, and the registry gate a
/// chat without a repository is recorded under.
struct ChatHome<'a> {
    repository: Option<Repository>,
    root_dir: Option<std::path::PathBuf>,
    _registry: Option<tokio::sync::MutexGuard<'a, ()>>,
}

/// Insert `chat` unless the repository or the workspace it runs in is being
/// removed, below the space `parent` when given, which must take it and, as
/// `removing` shows, not be a chat being removed.
fn insert(
    tx: &rusqlite::Transaction<'_>,
    mut chat: Chat,
    parent: Option<(SpaceId, super::spaces::Removing)>,
) -> Result<Chat> {
    let workspace = chat.workspace.as_ref().map(|workspace| &workspace.id);
    ensure_place_free(tx, chat.repository_id.as_ref(), workspace)?;
    tx.execute(
        "INSERT INTO chats(id,repository_id,name,agent,started_at,origin,scope_token,
            workspace_id,mode,root_dir,narrowing)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            chat.id,
            chat.repository_id,
            chat.name,
            chat.agent,
            chat.started_at,
            chat.origin
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            crate::daemon::scope::new_token(),
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

/// Rename the running or stopped chat `chat` to `name`. Returns the chat
/// before and after.
fn rename(
    tx: &rusqlite::Transaction<'_>,
    owner: &Path,
    chat: &SpaceId,
    name: &SpaceName,
) -> Result<(Chat, Chat)> {
    let old = recorded(tx, owner)?
        .into_iter()
        .find(|running| running.id == *chat)
        .with_context(|| format!("no chat {chat}"))?;
    if old.name != *name {
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

/// The running and stopped chats in start order, a chat without a
/// repository with its temporary directory in the `root_dir` it started in,
/// marked as `owner`'s, the daemon's state directory.
fn recorded(db: &Connection, owner: &Path) -> Result<Vec<Chat>> {
    db.prepare(
        "SELECT chats.id,chats.repository_id,chats.name,chats.agent,
                    EXISTS (SELECT 1 FROM executions e WHERE e.space_id=chats.id) AS running,
                    chats.started_at,chats.origin,repositories.workspaces_dir,chats.session,
                    space_completion.record AS completion,
                    workspaces.id,workspaces.name,workspaces.branch,chats.number,chats.mode,
                    chats.root_dir,chats.narrowing,chats.converted
             FROM chats LEFT JOIN repositories ON repositories.id=chats.repository_id
                LEFT JOIN workspaces ON workspaces.id=chats.workspace_id
                LEFT JOIN space_completion ON space_completion.space_id=chats.id
             ORDER BY chats.started_at, chats.rowid",
    )?
    .query_map([], |row| {
        let id: SpaceId = row.get(0)?;
        let repository_id: Option<RepositoryId> = row.get(1)?;
        let root_dir = row
            .get::<_, Option<String>>(15)?
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
            number: row.get(13)?,
            repository_id,
            name: row.get(2)?,
            agent: row.get(3)?,
            mode: row.get(14)?,
            narrowing: store::json_column(row, "narrowing")?.unwrap_or_default(),
            workspace: match row.get::<_, Option<SpaceId>>(10)? {
                Some(id) => Some(WorkspaceRef {
                    id,
                    name: row.get(11)?,
                    branch: row.get(12)?,
                }),
                None => None,
            },
            started_at: row.get(5)?,
            origin: store::json_column(row, "origin")?,
            tmp_dir,
            root_dir,
            session: row.get(8)?,
            running: row.get("running")?,
            links: SpaceLinks::default(),
            review: Vec::new(),
            // An unreadable record fails the cleanup that reads it, for that
            // chat alone, rather than every listing of chats.
            completion: (row.get::<_, Option<String>>("completion")?)
                .and_then(|record| serde_json::from_str(&record).ok()),
            agent_state: None,
            progress: None,
            sleep: None,
            asleep: None,
            parent: None,
            children: Vec::new(),
            converted: row.get(17)?,
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
        chat.sleep = crate::daemon::idle_wakes::sleep::current(db, &chat.id)?;
        chat.asleep = workspace::agent_queue::asleep::current(db, &chat.id)?;
        Ok(chat)
    })
    .collect()
}

/// Delete the record of the chat `id`, just started, which never ran, since
/// its start failed with `error`, journaling its removal as a removed
/// space's.
fn forget_unstarted(tx: &rusqlite::Transaction<'_>, id: &SpaceId, error: &str) -> Result<()> {
    let unstarted = store::exists(
        tx,
        "SELECT 1 FROM chats WHERE id=?1 AND NOT EXISTS
            (SELECT 1 FROM executions WHERE space_id=?1)",
        [id],
    )?;
    if unstarted {
        let cause = events::EventCause::StartFailed;
        events::record(tx, id, events::EventKind::Removed, cause, Some(error))?;
        tx.execute("DELETE FROM space_records WHERE id=?1", [id])?;
    }
    Ok(())
}

mod done;
mod files;
pub(crate) mod launches;
mod upgraded;

#[cfg(test)]
mod tests;
