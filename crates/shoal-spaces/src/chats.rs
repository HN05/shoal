//! Chats: agents the user talks with in a repository's registered checkout,
//! or in a workspace's worktree, where they are not the workspace's agent,
//! or without a repository in `root_dir`, managing the spaces of every
//! repository. A chat's agent runs under the execution wrapper as an
//! execution of the chat, which runs while its execution is recorded; the
//! chat lasts until it is removed.
mod root_moves;

use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::Records;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use std::path::{Path, PathBuf};

#[cfg(any(test, feature = "test-support"))]
use anyhow::Context as _;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    fsutil::off_runtime,
    model::{
        Chat, ChatPart, RefPart, Repository, SpaceKind, SpaceLinks, SpaceRef, Workdir, Workspace,
        WorkspaceRef,
    },
    {events, spaces, store},
};
use shoal_core::tmp_dirs;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Record the chat `new` in the checkout of `repository`, in the
    /// worktree of the workspace `workspace` names, which must be ready, in
    /// the workspace's repository, or without either in `root_dir`.
    pub async fn start_new_chat(
        &self,
        daemon: &dyn Daemon,
        repository: Option<crate::repository::Selector>,
        workspace: Option<&SpaceSelector>,
        mut new: NewChat,
    ) -> Result<Chat> {
        new.name = new.name.checked()?;
        let workspace = match workspace {
            Some(selector) => Some(self.chat_workspace(daemon, selector).await?),
            None => None,
        };
        let repository = match (&workspace, repository) {
            (Some(workspace), _) => workspace.repository_id.clone(),
            (None, Some(repository)) => Some(daemon.repository(repository).await?.id),
            (None, None) => None,
        };
        self.record_new_chat(daemon, repository, workspace.as_ref(), new)
            .await
    }

    /// Refuse a named `parent` that is `workspace`, whose worktree the chat
    /// runs in: the chat is not the workspace's child, whose threads would
    /// go to the workspace's agent while the user attends the chat.
    pub async fn ensure_parent_not_host(
        &self,
        daemon: &dyn Daemon,
        workspace: Option<&SpaceSelector>,
        parent: Option<&SpaceSelector>,
    ) -> Result<()> {
        let (Some(workspace), Some(parent)) = (workspace, parent) else {
            return Ok(());
        };
        let host = self.workspace(workspace).await?;
        let parent = self.parent_space_id(daemon, Some(parent)).await?;
        ensure!(
            parent.as_ref() != Some(&host.id),
            "a chat in the worktree of {} cannot go below it; name another --parent",
            host.name
        );
        Ok(())
    }

    /// The workspace `selector` names, once a chat may run in it: it is
    /// ready and its worktree is there and verified to be its own.
    pub async fn chat_workspace(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        ensure!(
            workspace.state == crate::state::WorkspaceState::Ready,
            "workspace {} is {}, and a chat runs only in a ready workspace",
            workspace.name,
            workspace.state
        );
        let path = &workspace.path;
        ensure!(
            self.file_access
                .checked(&self.notifications, path, off_runtime::is_dir(path))
                .await?,
            "the worktree of workspace {} is missing at {}",
            workspace.name,
            path.display()
        );
        self.file_access
            .checked(
                &self.notifications,
                path,
                self.verify_worktree(daemon, &workspace),
            )
            .await?;
        Ok(workspace)
    }

    /// Record `new` as a chat of the repository `repository`, or of none,
    /// in the worktree of `workspace` when given, below its parent, which
    /// must take a chat of that repository, or at the top without one.
    pub async fn record_new_chat(
        &self,
        daemon: &dyn Daemon,
        repository: Option<RepositoryId>,
        workspace: Option<&Workspace>,
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
        let home = self.reserve_chat_home(daemon, repository).await?;
        let started_at = crate::time::UnixTime::now();
        let chat = Chat {
            id: SpaceId::generate(),
            number: SpaceNumber::default(),
            repository_id: (home.repository.as_ref()).map(|repository| repository.id.clone()),
            name,
            mode: Some(mode),
            narrowing,
            running: false,
            links: SpaceLinks::default(),
            review: Vec::new(),
            holds: Vec::new(),
            agent_state: None,
            progress: None,
            sleep: None,
            wake_at: None,
            asleep: None,
            restart: None,
            parent: None,
            children: Vec::new(),
            workdir: home.workdir(workspace)?,
            part: ChatPart {
                agent: Some(agent),
                started_at,
                tmp_dir: None,
                completion: None,
            },
        };
        self.record_chat(daemon, chat, home, parent, priority, origin)
            .await
    }

    /// Record the removed chat `chat` again, as its restore reopens it: in
    /// its repository, or else in the `root_dir` it ran in, below `parent`
    /// when given.
    pub async fn record_restored_chat(
        &self,
        daemon: &dyn Daemon,
        chat: Chat,
        parent: Option<SpaceId>,
    ) -> Result<Chat> {
        let home = match &chat.repository_id {
            Some(id) => ChatHome {
                repository: Some(
                    daemon
                        .repository(crate::repository::Selector::from(id))
                        .await?,
                ),
                root_dir: None,
                _registry: None,
            },
            None => ChatHome {
                repository: None,
                root_dir: chat.workdir.root_dir().map(Into::into),
                _registry: Some(self.gates.lock_registry().await),
            },
        };
        self.record_chat(daemon, chat, home, parent, None, None)
            .await
    }

    /// Record `chat` at `home`, below `parent` when given, with `priority`,
    /// give it its temporary directory and take `place`, where it runs in a
    /// terminal host, when given.
    async fn record_chat(
        &self,
        daemon: &dyn Daemon,
        chat: Chat,
        home: ChatHome<'_>,
        parent: Option<SpaceId>,
        priority: Option<shoal_schedule::priority::Priority>,
        place: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        ensure!(
            !daemon.agents_suspended(),
            "Shoal is stopping work for a shutdown or a daemon stop; start the chat once it is over"
        );
        let below = parent.map(|parent| (parent, self.space_removals.clone()));
        let records = self.records.clone();
        let chat = self
            .store
            .write(move |tx| {
                let chat = insert(tx, &*records, chat, below)?;
                if let Some(priority) = priority {
                    super::spaces::priority::set(tx, &chat.id, priority)?;
                }
                Ok(chat)
            })
            .await?;
        let chat = (self.give_tmp_dir(chat, home.repository.as_ref())).await?;
        if let Some(place) = place {
            daemon.take_chat_place(&chat.id, place).await?;
        }
        Ok(chat)
    }

    /// Where a new chat of `repository` keeps its temporary directory: the
    /// repository's directory, or for a chat without one the
    /// configured `root_dir`, which stays its own however `root_dir` changes
    /// later. A chat without one is recorded under the registry gate, so a
    /// root move, which holds it, finds the chat before clearing `root_dir`.
    async fn reserve_chat_home(
        &self,
        daemon: &dyn Daemon,
        repository: Option<RepositoryId>,
    ) -> Result<ChatHome<'_>> {
        Ok(match repository {
            Some(id) => ChatHome {
                repository: Some(
                    daemon
                        .repository(crate::repository::Selector::from(&id))
                        .await?,
                ),
                root_dir: None,
                _registry: None,
            },
            None => ChatHome {
                repository: None,
                _registry: Some(self.gates.lock_registry().await),
                root_dir: Some(self.config.current().root_dir(&self.paths)?),
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

    /// Running and stopped chats in start order, each with its agent.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        let recorded = self.recorded_chats(None).await?;
        Ok(self.with_agents(recorded))
    }

    /// The running or stopped chat with the ID `id`, with its agent.
    pub async fn chat_by_id(&self, id: &SpaceId) -> Result<Option<Chat>> {
        let recorded = self.recorded_chats(Some(id)).await?;
        (recorded.into_iter())
            .find(|(chat, _)| chat.id == *id)
            .map(|(chat, stored)| self.with_agent(chat, stored))
            .transpose()
    }

    /// The running and stopped chats in start order, every one or those
    /// at or below the space `root`, each with what the store records of
    /// its agent, read in one transaction.
    async fn recorded_chats(
        &self,
        root: Option<&SpaceId>,
    ) -> Result<Vec<(Chat, spaces::sessions::StoredAgents)>> {
        let (root, owner) = (root.cloned(), self.root_dir_owner());
        let records = self.records.clone();
        self.store
            .read(move |tx| recorded_within(tx, &*records, &owner, root.as_ref()))
            .await
    }

    /// The `recorded` chats, each with its agent, of which the store
    /// records what each comes with. One whose saved agent cannot be read
    /// is listed without it, so it costs only its own chat.
    pub fn with_agents(&self, recorded: Vec<(Chat, spaces::sessions::StoredAgents)>) -> Vec<Chat> {
        (recorded.into_iter())
            .map(|(chat, stored)| self.listed_with_agent(chat, stored))
            .collect()
    }

    /// `chat` with its agent, of which the store records `stored`, or
    /// without one when what its stop saved cannot be read.
    fn listed_with_agent(&self, mut chat: Chat, stored: spaces::sessions::StoredAgents) -> Chat {
        chat.agent = self.agent_from(&chat.id, stored).unwrap_or_else(|error| {
            crate::log!("saved agent of chat {} unreadable: {error:#}", chat.id);
            None
        });
        chat
    }

    /// `chat` with its agent, of which the store records `stored`.
    fn with_agent(&self, chat: Chat, stored: spaces::sessions::StoredAgents) -> Result<Chat> {
        let mut chat = chat;
        chat.agent = self.agent_from(&chat.id, stored)?;
        Ok(chat)
    }

    /// The chats running or stopped in the worktree of the workspace `id`,
    /// by name.
    pub async fn chats_in(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
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
                            part: RefPart::Chat,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The workspace in whose worktree the space `id` runs, when it is a
    /// chat there.
    pub async fn chat_home(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
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
    pub async fn chat_runs_in(&self, id: &SpaceId) -> Result<bool> {
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

    /// The registered checkout of `chat`'s repository; none for a chat
    /// without a repository.
    pub async fn chat_repository(
        &self,
        daemon: &dyn Daemon,
        chat: &Chat,
    ) -> Result<Option<Repository>> {
        match &chat.repository_id {
            Some(id) => Ok(Some(
                daemon
                    .repository(crate::repository::Selector::from(id))
                    .await?,
            )),
            None => Ok(None),
        }
    }

    /// The directory a chat outside a workspace runs in: its repository's
    /// registered checkout, or the `root_dir` a chat without a repository
    /// started in.
    pub async fn chat_checkout_path(
        &self,
        daemon: &dyn Daemon,
        chat: &Chat,
    ) -> Result<std::path::PathBuf> {
        match (
            self.chat_repository(daemon, chat).await?,
            chat.workdir.root_dir(),
        ) {
            (Some(repository), _) => Ok(repository.path),
            (None, Some(root_dir)) => Ok(root_dir.to_owned()),
            (None, None) => self.config.current().root_dir(&self.paths),
        }
    }

    /// The layout of the temporary directories of the chats without a
    /// repository in the configured `root_dir`, which no other daemon uses or
    /// sweeps; none while `root_dir` is not valid, when no such chat starts
    /// either.
    pub fn outside_tmp_layout(&self) -> Option<tmp_dirs::Layout> {
        let root_dir = self.config.current().root_dir(&self.paths).ok()?;
        Some(tmp_dirs::Layout::outside_repositories(
            &root_dir,
            &self.root_dir_owner(),
        ))
    }

    /// The state directory as the daemon marks what it keeps in `root_dir`:
    /// canonical, so every path to it names the same owner.
    pub fn root_dir_owner(&self) -> std::path::PathBuf {
        std::fs::canonicalize(&self.paths.state).unwrap_or_else(|_| self.paths.state.clone())
    }

    /// The temporary directory of `chat` of `repository`, or of none,
    /// created when missing.
    pub fn chat_tmp_dir(
        &self,
        repository: Option<&Repository>,
        chat: &Chat,
    ) -> Result<Option<std::path::PathBuf>> {
        (self.tmp_layout(repository, chat.workdir.root_dir()))
            .map(|layout| layout.prepare(&chat.id))
            .transpose()
    }

    /// The scope token the agent of the chat `id` carries.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn chat_scope(
        &self,
        id: &SpaceId,
    ) -> Result<shoal_protocol::scope_tokens::ScopeToken> {
        let id = id.to_owned();
        let token = (self.store)
            .run(move |db| shoal_coordination::scope::tokens::agent_token(db, &id))
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

/// Where a new chat keeps its temporary directory, and the registry gate a
/// chat without a repository is recorded under.
struct ChatHome<'a> {
    repository: Option<Repository>,
    root_dir: Option<std::path::PathBuf>,
    _registry: Option<tokio::sync::MutexGuard<'a, ()>>,
}

impl ChatHome<'_> {
    /// Where a chat here works: the worktree of `workspace` when given,
    /// else the repository's checkout, else the `root_dir`.
    fn workdir(&self, workspace: Option<&Workspace>) -> Result<Workdir> {
        Ok(match (workspace, &self.repository, &self.root_dir) {
            (Some(workspace), ..) => Workdir::Worktree {
                workspace: workspace.reference(),
                path: workspace.path.clone(),
            },
            (None, Some(repository), _) => Workdir::Checkout {
                path: repository.path.clone(),
            },
            (None, None, Some(root_dir)) => Workdir::RootDir {
                path: root_dir.clone(),
            },
            (None, None, None) => bail!("a chat without a repository needs a root_dir"),
        })
    }
}

/// Insert `chat` unless the repository or the workspace it runs in is being
/// removed, below the space `parent` when given, which must take it and, as
/// `removing` shows, not be a chat being removed.
fn insert(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    mut chat: Chat,
    parent: Option<(SpaceId, super::spaces::Removing)>,
) -> Result<Chat> {
    let workspace = chat.workdir.workspace().map(|workspace| &workspace.id);
    ensure_place_free(tx, chat.repository_id.as_ref(), workspace)?;
    // Checked with the record, so a removal that began while the start
    // waited finds the chat below the parent or refuses it; recorded first,
    // so the chat's start names it.
    if let Some((parent, removing)) = &parent {
        ensure!(!removing.reserved(parent), "{parent} is being removed");
        super::tree::place_below(tx, chat.repository_id.as_ref(), &chat.id, parent)?;
    }
    tx.execute(
        "INSERT INTO chats(id,workspace_id,root_dir) VALUES (?1,?2,?3)",
        params![
            chat.id,
            workspace,
            (chat.workdir.root_dir()).map(|root| root.to_string_lossy().into_owned()),
        ],
    )?;
    chat.number = spaces::records::record(
        tx,
        records,
        &spaces::records::NewSpace {
            id: &chat.id,
            kind: SpaceKind::Chat,
            repository_id: chat.repository_id.as_ref(),
            name: &chat.name,
            number: SpaceNumber::default(),
            mode: chat.mode.as_ref(),
            narrowing: &chat.narrowing,
            created_at: chat.started_at,
        },
    )?;
    shoal_coordination::scope::tokens::issue_agent(tx, &chat.id)?;
    if parent.is_some() {
        chat.parent = super::tree::parent(tx, &chat.id)?;
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

/// The running and stopped chats in start order, every one or those at or
/// below the space `root`, each with what the store records of its agent;
/// `owner` owns the `root_dir` of a chat without a repository.
pub fn recorded_within(
    db: &Connection,
    records: &dyn Records,
    owner: &Path,
    root: Option<&SpaceId>,
) -> Result<Vec<(Chat, spaces::sessions::StoredAgents)>> {
    (recorded(db, records, owner, root)?.into_iter())
        .map(|chat| {
            let stored = spaces::sessions::stored_agents(db, &chat.id)?;
            Ok((chat, stored))
        })
        .collect()
}

/// The running and stopped chats in start order, every one or those at or
/// below the space `root`, a chat without a repository with its temporary
/// directory in the `root_dir` it started in, marked as `owner`'s, the
/// daemon's state directory, without their agents, which
/// [`Manager::agent_of`] reads.
fn recorded(
    db: &Connection,
    records: &dyn Records,
    owner: &Path,
    root: Option<&SpaceId>,
) -> Result<Vec<Chat>> {
    let reached = root
        .map(|root| super::tree::subtree(db, root))
        .transpose()?;
    db.prepare(
        "SELECT chats.id,space.repository_id,space.name,
                    EXISTS (SELECT 1 FROM executions e WHERE e.space_id=chats.id) AS running,
                    space.created_at AS started_at,repositories.workspaces_dir,
                    space_completion.record AS completion,
                    workspaces.id AS workspace_id,home.name AS workspace_name,
                    workspaces.branch,workspaces.path AS worktree,repositories.path AS checkout,
                    space.number,space.mode,chats.root_dir,space.narrowing
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
        let root_dir = row.get::<_, Option<String>>("root_dir")?.map(PathBuf::from);
        let directory = row.get::<_, Option<String>>("workspaces_dir")?;
        let tmp_dir = match &repository_id {
            Some(repository_id) => directory.map(|directory| {
                tmp_dirs::Layout::new(Path::new(&directory), repository_id).locate(&id)
            }),
            None => (root_dir.as_deref())
                .map(|root| tmp_dirs::Layout::outside_repositories(root, owner).locate(&id)),
        };
        Ok(Chat {
            id,
            number: row.get("number")?,
            repository_id,
            name: row.get("name")?,
            mode: row.get("mode")?,
            narrowing: store::json_column(row, "narrowing")?.unwrap_or_default(),
            running: row.get("running")?,
            links: SpaceLinks::default(),
            review: Vec::new(),
            holds: Vec::new(),
            agent_state: None,
            progress: None,
            sleep: None,
            wake_at: None,
            asleep: None,
            restart: None,
            parent: None,
            children: Vec::new(),
            workdir: workdir(row, root_dir)?,
            part: ChatPart {
                agent: None,
                started_at: row.get("started_at")?,
                tmp_dir,
                // An unreadable record fails the cleanup that reads it, for
                // that chat alone, rather than every listing of chats.
                completion: (row.get::<_, Option<String>>("completion")?)
                    .and_then(|record| serde_json::from_str(&record).ok()),
            },
        })
    })?
    .collect::<rusqlite::Result<Vec<Chat>>>()?
    .into_iter()
    .filter(|chat| (reached.as_ref()).is_none_or(|reached| reached.contains(&chat.id)))
    .map(|mut chat| {
        chat.review = crate::ready_marks::list(db, &chat.id)?;
        chat.holds = crate::holds::list(db, &chat.id)?;
        chat.parent = super::tree::parent(db, &chat.id)?;
        chat.children = super::tree::children(db, &chat.id)?;
        chat.progress = crate::progress::current(db, &chat.id)?;
        records.load_chat(db, &mut chat)?;
        Ok(chat)
    })
    .collect()
}

/// Where the chat of `row` works: the worktree it runs in, its repository's
/// checkout, or the `root_dir` it started in.
fn workdir(row: &rusqlite::Row<'_>, root_dir: Option<PathBuf>) -> rusqlite::Result<Workdir> {
    if let Some(id) = row.get::<_, Option<SpaceId>>("workspace_id")? {
        return Ok(Workdir::Worktree {
            workspace: WorkspaceRef {
                id,
                name: row.get("workspace_name")?,
                branch: row.get("branch")?,
            },
            path: PathBuf::from(row.get::<_, String>("worktree")?),
        });
    }
    if let Some(checkout) = row.get::<_, Option<String>>("checkout")? {
        return Ok(Workdir::Checkout {
            path: PathBuf::from(checkout),
        });
    }
    let path = root_dir.ok_or_else(|| {
        let error = anyhow::anyhow!("a chat without a repository records no root_dir");
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Null, error.into())
    })?;
    Ok(Workdir::RootDir { path })
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
        let cause = crate::model::EventCause::StartFailed;
        events::record(tx, id, events::EventKind::Removed, cause, Some(error))?;
        spaces::records::remove(tx, id)?;
    }
    Ok(())
}
