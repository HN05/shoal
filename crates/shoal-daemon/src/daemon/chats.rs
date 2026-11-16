//! Chats: agents the user talks with in a repository's registered checkout,
//! or in a workspace's worktree, where they are not the workspace's agent,
//! or without a repository in `root_dir`, managing the spaces of every
//! repository. A chat's agent runs under the execution wrapper as an
//! execution of the chat, which runs while its execution is recorded; the
//! chat lasts until it is removed.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::scope_tokens::ScopeToken;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    daemon::{
        events,
        scope::Caller,
        spaces, store,
        workspace::{self, Manager},
    },
    fsutil::off_runtime,
    model::{
        Chat, RenamedChat, Repository, SpaceKind, SpaceLinks, SpaceRef, Workspace, WorkspaceRef,
    },
    tmp_dirs,
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

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
            origin: origin.clone(),
            parent: None,
        };
        let chat = self.start_new_chat(Some(repository), None, new).await?;
        self.run_chat_agent(chat, origin).await
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
            origin: origin.clone(),
            parent: None,
        };
        let repository = Some(workspace.repository_id.clone());
        let chat = (self.record_new_chat(repository, Some(workspace_ref(&workspace)), new)).await?;
        self.run_chat_agent(chat, origin).await
    }

    /// Run the agent of `chat` as its execution at `origin`, as its wrapper
    /// registers and tracks it, and return the chat running it. Asked to
    /// stop, the agent ends its processes and exits, as its wrapper stops
    /// it, first saving itself for `shoal restore` when the stop asks that.
    #[cfg(test)]
    pub(crate) async fn run_chat_agent(
        self: &std::sync::Arc<Self>,
        chat: Chat,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let place = origin.map(|origin| workspace::places::ExecutionPlace {
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
            name: chat.agent.clone().context("the chat names no agent")?,
            model: Default::default(),
            mode: chat.mode.clone(),
            narrowing: chat.narrowing,
        };
        let id = started.plan.id.clone();
        let running = std::time::Duration::ZERO;
        (self.track_agent(&id, &agent, &chat.id, &chat.name, true, running)).await;
        let (manager, space) = (self.clone(), chat.id.clone());
        tokio::spawn(async move {
            if started.stop.wait_for(|stop| *stop).await.is_ok() {
                let request = manager.executions.stop_request(&id).await;
                if let Some(request) = request.filter(|request| request.saves(true)) {
                    use crate::execution::recovery::{self, RecordExt};
                    let stage = shoal_protocol::executions::Stage::Stopped;
                    let record = recovery::Record::new(&agent, Some(&request.reason), stage, None);
                    recovery::save_stopped(&manager.paths, &space, &id, record, None).unwrap();
                }
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

    /// Refuse the permissions `given` to a new chat in the repository or
    /// the workspace of `place` where the scoped `caller` does not hold
    /// them.
    pub(crate) async fn ensure_chat_given_held(
        &self,
        caller: Option<&Caller>,
        given: &crate::config::permissions::PermissionsConfig,
        place: (Option<&crate::repository::Selector>, Option<&SpaceSelector>),
    ) -> Result<()> {
        let Some(caller) = caller.filter(|_| !given.is_empty()) else {
            return Ok(());
        };
        let repository = match place {
            (_, Some(workspace)) => Some(self.chat_workspace(workspace).await?.repository_id),
            (Some(repository), None) => Some(self.repository(repository.clone()).await?.id),
            (None, None) => None,
        };
        let giver = Some(&caller.space_id);
        (self.ensure_caller_holds(giver, given, repository.as_ref())).await
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
        let started_at = crate::time::UnixTime::now();
        let chat = Chat {
            id: SpaceId::generate(),
            number: SpaceNumber::default(),
            repository_id: (home.repository.as_ref()).map(|repository| repository.id.clone()),
            name,
            agent: Some(agent),
            mode,
            narrowing,
            workspace,
            started_at,
            tmp_dir: None,
            root_dir: home.root_dir.clone(),
            running: false,
            links: SpaceLinks::default(),
            review: Vec::new(),
            holds: Vec::new(),
            completion: None,
            agent_state: None,
            progress: None,
            sleep: None,
            asleep: None,
            parent: None,
            children: Vec::new(),
        };
        self.record_chat(chat, home, parent, priority, origin).await
    }

    /// Record the removed chat `chat` again, as its restore reopens it: in
    /// its repository, or else in the `root_dir` it ran in, below `parent`
    /// when given.
    pub(crate) async fn record_restored_chat(
        &self,
        chat: Chat,
        parent: Option<SpaceId>,
    ) -> Result<Chat> {
        let home = match &chat.repository_id {
            Some(id) => ChatHome {
                repository: Some(self.reserve_repository_directory(id).await?),
                root_dir: None,
                _registry: None,
            },
            None => ChatHome {
                repository: None,
                root_dir: chat.root_dir.clone(),
                _registry: Some(self.gates.lock_registry().await),
            },
        };
        self.record_chat(chat, home, parent, None, None).await
    }

    /// Record `chat` at `home`, below `parent` when given, with `priority`,
    /// give it its temporary directory and take `place`, where it runs in a
    /// terminal host, when given.
    async fn record_chat(
        &self,
        chat: Chat,
        home: ChatHome<'_>,
        parent: Option<SpaceId>,
        priority: Option<shoal_schedule::priority::Priority>,
        place: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        ensure!(
            !self.agent_queue.suspended(),
            "Shoal is stopping work for a shutdown or a daemon stop; start the chat once it is over"
        );
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
        if let Some(place) = place {
            self.take_chat_place(&chat.id, place).await?;
        }
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
        let (old, mut renamed) = self
            .store
            .write(move |tx| rename(tx, &owner, &chat, &name))
            .await?;
        renamed.agent = self.agent_of(&renamed.id).await?;
        Ok(RenamedChat {
            chat: renamed,
            previous_name: old.name,
        })
    }

    /// Running and stopped chats in start order, each with its agent.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        let recorded = self.recorded_chats().await?;
        (recorded.into_iter())
            .map(|(chat, stored)| self.with_agent(chat, stored))
            .collect()
    }

    /// The running or stopped chat with the ID `id`, with its agent.
    pub(crate) async fn chat_by_id(&self, id: &SpaceId) -> Result<Option<Chat>> {
        let recorded = self.recorded_chats().await?;
        (recorded.into_iter())
            .find(|(chat, _)| chat.id == *id)
            .map(|(chat, stored)| self.with_agent(chat, stored))
            .transpose()
    }

    /// The running and stopped chats in start order, each with what the
    /// store records of its agent, read in one transaction.
    async fn recorded_chats(&self) -> Result<Vec<(Chat, spaces::sessions::StoredAgents)>> {
        let owner = self.root_dir_owner();
        self.store
            .read(move |tx| {
                (recorded(tx, &owner)?.into_iter())
                    .map(|chat| {
                        let stored = spaces::sessions::stored_agents(tx, &chat.id)?;
                        Ok((chat, stored))
                    })
                    .collect()
            })
            .await
    }

    /// `chat` with its agent, of which the store records `stored`.
    fn with_agent(&self, chat: Chat, stored: spaces::sessions::StoredAgents) -> Result<Chat> {
        let agent = self.agent_from(&chat.id, stored)?;
        Ok(Chat { agent, ..chat })
    }

    /// The chats running or stopped in the worktree of the workspace `id`,
    /// by name.
    pub(crate) async fn chats_in(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,name FROM spaces JOIN chats USING(id)
                         WHERE workspace_id=?1 ORDER BY name",
                    )?
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
        self.space_settings_in(chat.repository_id.as_ref()).await
    }

    /// The settings of a space of the repository `id`, or the global
    /// config's alone for a space without one.
    pub(crate) async fn space_settings_in(
        &self,
        id: Option<&RepositoryId>,
    ) -> Result<crate::config::Effective> {
        match id {
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
        let token = (self.store)
            .run(move |db| crate::daemon::scope::tokens::chat_token(db, &id))
            .await?;
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
    pub priority: Option<shoal_schedule::priority::Priority>,
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
    // Checked with the record, so a removal that began while the start
    // waited finds the chat below the parent or refuses it; recorded first,
    // so the chat's start names it.
    if let Some((parent, removing)) = &parent {
        ensure!(!removing.reserved(parent), "{parent} is being removed");
        workspace::ensure_parent(tx, chat.repository_id.as_ref(), &chat.id, parent)?;
        workspace::set_parent(tx, &chat.id, Some(parent))?;
    }
    tx.execute(
        "INSERT INTO chats(id,workspace_id,root_dir) VALUES (?1,?2,?3)",
        params![
            chat.id,
            workspace,
            (chat.root_dir.as_deref()).map(|root| root.to_string_lossy().into_owned()),
        ],
    )?;
    chat.number = spaces::records::record(
        tx,
        &spaces::records::NewSpace {
            id: &chat.id,
            kind: SpaceKind::Chat,
            repository_id: chat.repository_id.as_ref(),
            name: &chat.name,
            number: SpaceNumber::default(),
            mode: Some(&chat.mode),
            narrowing: &chat.narrowing,
            created_at: chat.started_at,
        },
    )?;
    crate::daemon::scope::tokens::issue(tx, &chat.id)?;
    if parent.is_some() {
        chat.parent = workspace::parent(tx, &chat.id)?;
    }
    Ok(chat)
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
            "UPDATE spaces SET name=?2 WHERE id=?1",
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
/// marked as `owner`'s, the daemon's state directory, without their agents,
/// which [`Manager::agent_of`] reads.
fn recorded(db: &Connection, owner: &Path) -> Result<Vec<Chat>> {
    db.prepare(
        "SELECT chats.id,space.repository_id,space.name,
                    EXISTS (SELECT 1 FROM executions e WHERE e.space_id=chats.id) AS running,
                    space.created_at AS started_at,repositories.workspaces_dir,
                    space_completion.record AS completion,
                    workspaces.id AS workspace_id,home.name AS workspace_name,
                    workspaces.branch,space.number,space.mode,chats.root_dir,space.narrowing
             FROM chats JOIN spaces space ON space.id=chats.id
                LEFT JOIN repositories ON repositories.id=space.repository_id
                LEFT JOIN workspaces ON workspaces.id=chats.workspace_id
                LEFT JOIN spaces home ON home.id=workspaces.id
                LEFT JOIN space_completion ON space_completion.space_id=chats.id
             ORDER BY space.created_at, chats.rowid",
    )?
    .query_map([], |row| {
        let id: SpaceId = row.get("id")?;
        let repository_id: Option<RepositoryId> = row.get("repository_id")?;
        let root_dir = row
            .get::<_, Option<String>>("root_dir")?
            .map(std::path::PathBuf::from);
        let directory = row.get::<_, Option<String>>("workspaces_dir")?;
        let tmp_dir = match (&repository_id, directory) {
            (Some(repository_id), Some(directory)) => {
                Some(tmp_dirs::Layout::new(Path::new(&directory), repository_id).locate(&id))
            }
            (Some(_), None) => None,
            (None, _) => (root_dir.as_deref())
                .map(|root| tmp_dirs::Layout::outside_repositories(root, owner).locate(&id)),
        };
        Ok(Chat {
            id,
            number: row.get("number")?,
            repository_id,
            name: row.get("name")?,
            agent: None,
            mode: row.get("mode")?,
            narrowing: store::json_column(row, "narrowing")?.unwrap_or_default(),
            workspace: match row.get::<_, Option<SpaceId>>("workspace_id")? {
                Some(id) => Some(WorkspaceRef {
                    id,
                    name: row.get("workspace_name")?,
                    branch: row.get("branch")?,
                }),
                None => None,
            },
            started_at: row.get("started_at")?,
            tmp_dir,
            root_dir,
            running: row.get("running")?,
            links: SpaceLinks::default(),
            review: Vec::new(),
            holds: Vec::new(),
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
        })
    })?
    .collect::<rusqlite::Result<Vec<Chat>>>()?
    .into_iter()
    .map(|mut chat| {
        chat.links = store::space_links(db, &chat.id)?;
        chat.review = crate::daemon::workspace::review::list(db, &chat.id)?;
        chat.holds = crate::daemon::workspace::holds::list(db, &chat.id)?;
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
        "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat' AND NOT EXISTS
            (SELECT 1 FROM executions WHERE space_id=?1)",
        [id],
    )?;
    if unstarted {
        let cause = events::EventCause::StartFailed;
        events::record(tx, id, events::EventKind::Removed, cause, Some(error))?;
        spaces::records::remove(tx, id)?;
    }
    Ok(())
}

mod done;
mod files;
pub(crate) mod launches;

#[cfg(test)]
mod tests;
