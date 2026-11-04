//! Chats: agents the user talks with in a repository's registered checkout,
//! or in a workspace's worktree, where they are not the workspace's agent.
//! They are records, not tracked executions, so they never queue or stop
//! under pressure. A chat runs while its process lives; once its agent exits
//! or Shoal saves it, the chat is stopped until it is restored, and it lasts
//! until it is removed.
use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{
    daemon::{
        approvals::{ApprovalRequest, release_decided_by},
        log, store,
        workspace::{self, Manager, derive_workspace_name},
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
        name: String,
        agent: String,
        pid: u32,
        origin: Option<crate::sessions::Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let new = NewChat {
            name,
            agent,
            pid,
            origin,
            hands_over,
            parent: None,
        };
        self.start_new_chat(repository, None, new).await
    }

    /// [`Self::start_chat`] in the worktree of the workspace `workspace`
    /// names, which must be ready, in the workspace's repository.
    #[cfg(test)]
    pub async fn start_chat_in_workspace(
        &self,
        workspace: &str,
        name: String,
        agent: String,
        pid: u32,
        origin: Option<crate::sessions::Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let workspace = self.chat_workspace(workspace).await?;
        let new = NewChat {
            name,
            agent,
            pid,
            origin,
            hands_over,
            parent: None,
        };
        let repository = workspace.repository_id.clone();
        self.record_new_chat(repository, Some(workspace_ref(&workspace)), new)
            .await
    }

    /// Record the chat `new` in the repository's checkout, or in the
    /// worktree of the workspace `workspace` names, which must be ready, in
    /// the workspace's repository.
    pub async fn start_new_chat(
        &self,
        repository: crate::repository::Selector,
        workspace: Option<&str>,
        new: NewChat,
    ) -> Result<Chat> {
        let (repository, workspace) = match workspace {
            Some(selector) => {
                let workspace = self.chat_workspace(selector).await?;
                (
                    workspace.repository_id.clone(),
                    Some(workspace_ref(&workspace)),
                )
            }
            None => (self.repository(repository).await?.id, None),
        };
        self.record_new_chat(repository, workspace, new).await
    }

    /// The workspace `selector` names, once a chat may run in it: it is
    /// ready and its worktree is there and verified to be its own.
    pub(crate) async fn chat_workspace(&self, selector: &str) -> Result<Workspace> {
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

    /// Record `new` as a chat of the repository `repository`, in the
    /// worktree of `workspace` when given, below its parent, which must be
    /// in the repository, or at the top without one.
    async fn record_new_chat(
        &self,
        repository: String,
        workspace: Option<WorkspaceRef>,
        new: NewChat,
    ) -> Result<Chat> {
        let NewChat {
            name,
            agent,
            pid,
            origin,
            hands_over,
            parent,
        } = new;
        crate::validate::name("chat", &name)?;
        // Its temporary directory goes in the repository's directory.
        let repository = self.reserve_repository_directory(&repository).await?;
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        // A save under way would miss it, and a shutdown's would not come again.
        let _saving = self.chat_saves.lock().await;
        ensure!(
            !self.agent_queue.suspended(),
            "Shoal is stopping work for a shutdown or a daemon stop; start the chat once it is over"
        );
        let started_at = i64::try_from(crate::time::unix_seconds())?;
        // A place Shoal created for a workspace is the chat's now.
        if let Some(origin) = &origin {
            self.release_place(origin).await?;
        }
        let chat = Chat {
            id: Uuid::new_v4().to_string(),
            number: 0,
            repository_id: repository.id.clone(),
            name,
            agent,
            workspace,
            pid: process.pid,
            started_at,
            origin,
            tmp_dir: None,
            session: None,
            restore: None,
            links: SpaceLinks::default(),
            review: Vec::new(),
            completion: None,
            parent: None,
            children: Vec::new(),
        };
        let (recorded, removing) = (process.clone(), self.chat_removals.clone());
        let below = parent.map(|parent| (parent, removing));
        let chat = self
            .store
            .run(move |db| insert(db, chat, &recorded, hands_over, below))
            .await?;
        self.give_tmp_dir(chat, &repository, process).await
    }

    /// `chat`, just recorded with `process`, with its temporary directory,
    /// created now; a chat that cannot have one is forgotten, having never
    /// run.
    async fn give_tmp_dir(
        &self,
        mut chat: Chat,
        repository: &Repository,
        process: Identity,
    ) -> Result<Chat> {
        match tmp_dirs::directory(repository, &chat.id) {
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
    pub async fn check_chat_name(&self, name: String) -> Result<()> {
        crate::validate::name("chat", &name)?;
        self.store
            .run(move |db| store::ensure_name_available(db, &name, None))
            .await
    }

    /// `name`, made a valid name, or its lowest `-2`, `-3`, ... variant
    /// that a new chat could take now.
    pub async fn free_chat_name(&self, name: String) -> Result<String> {
        let base = derive_workspace_name(&name);
        self.store.run(move |db| store::free_name(db, &base)).await
    }

    /// Give the chat `chat`, by ID or name, the name `name`, which must be
    /// free as a new chat's, and move its files to that name.
    pub async fn rename_chat(&self, chat: String, name: String) -> Result<RenamedChat> {
        crate::validate::name("chat", &name)?;
        let paths = self.paths.clone();
        // The store's one thread moves the files after the rename commits,
        // so no other rename or chat start reaches either name in between.
        self.store
            .run(move |db| {
                let (old, renamed) = rename(db, &chat, &name)?;
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
    pub async fn move_chat(&self, chat: &str, pid: u32) -> Result<Chat> {
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let chat = chat.to_owned();
        let _handoff = self.chat_handoffs.write().await;
        self.store.run(move |db| move_to(db, &chat, &process)).await
    }

    /// Record `session` as the agent session the running chat `id` continues.
    pub(crate) async fn record_chat_session(&self, id: &str, session: String) -> Result<()> {
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
        self.store.run(|db| recorded(db)).await
    }

    /// The agent processes of the running chats.
    pub(crate) async fn chat_processes(&self) -> Result<Vec<Identity>> {
        let recorded = self.store.run(|db| recorded_processes(db)).await?;
        Ok(recorded.into_iter().map(|(_, process)| process).collect())
    }

    /// The running chat `selector` names, by ID or name.
    pub(crate) async fn running_chat(&self, selector: &str) -> Result<Option<Chat>> {
        Ok(self
            .recorded_chat(selector)
            .await?
            .filter(|chat| chat.restore.is_none()))
    }

    /// The running or stopped chat `selector` names, by ID or name.
    pub(crate) async fn recorded_chat(&self, selector: &str) -> Result<Option<Chat>> {
        Ok(self
            .chats()
            .await?
            .into_iter()
            .find(|chat| chat.id == selector || chat.name == selector))
    }

    /// Stop the running chat `id`: stop every process it owns, so none goes
    /// on writing the agent's session, then record the chat stopped, as when
    /// the process it runs as exits. Until the stop finishes, the chat stays
    /// running, so nothing restores it meanwhile.
    pub(crate) async fn stop_chat(&self, id: &str) -> Result<()> {
        self.stop_chat_recording(id, |found| self.record_chat_stopping(id, found))
            .await
    }

    /// [`Self::stop_chat`], with `record` keeping what each round of the
    /// stop signals before it does.
    async fn stop_chat_recording<R>(
        &self,
        id: &str,
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
    pub(crate) async fn chat_saved(&self, id: &str) -> Result<bool> {
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
    pub(crate) async fn chats_in(&self, id: &str) -> Result<Vec<SpaceRef>> {
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
    pub(crate) async fn chat_home(&self, id: &str) -> Result<Option<String>> {
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
    pub(crate) async fn chat_runs_in(&self, id: &str) -> Result<bool> {
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
    pub(crate) async fn chat_saved_in(&self, id: &str) -> Result<bool> {
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

    /// When the chat `id`'s agent stopped, in Unix seconds, while it is
    /// stopped.
    pub(crate) async fn chat_stopped_at(&self, id: &str) -> Result<Option<u64>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let stopped_at: Option<Option<i64>> = db
                    .query_row("SELECT stopped_at FROM chats WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                Ok(stopped_at.flatten().map(|at| at.max(0) as u64))
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
        let released = self.store.run(move |db| stop(db, &exited)).await?;
        self.ask_user_about(&released).await;
        Ok(())
    }

    /// The scope token the agent of the chat `id` carries.
    pub(crate) async fn chat_scope(&self, id: &str) -> Result<String> {
        let id = id.to_owned();
        let token = self.store.run(move |db| scope_token(db, &id)).await?;
        token.context("no such chat")
    }
}

/// What a process records to start a chat as itself: the chat's name and
/// agent, where it runs in a terminal host, whether it `hands_over` to a
/// session it starts, and the space it goes below.
pub struct NewChat {
    pub name: String,
    pub agent: String,
    pub pid: u32,
    pub origin: Option<crate::sessions::Origin>,
    pub hands_over: bool,
    pub parent: Option<String>,
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
fn insert(
    db: &mut Connection,
    mut chat: Chat,
    process: &Identity,
    hands_over: bool,
    parent: Option<(String, Removing)>,
) -> Result<Chat> {
    let tx = db.transaction()?;
    let workspace = chat
        .workspace
        .as_ref()
        .map(|workspace| workspace.id.as_str());
    ensure_place_free(&tx, &chat.repository_id, workspace)?;
    store::ensure_name_available(&tx, &chat.name, None)?;
    tx.execute(
        "INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at,origin,starting,
            unique_id,scope_token,workspace_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
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
            Uuid::new_v4().to_string(),
            workspace
        ],
    )?;
    chat.number = tx.query_row("SELECT number FROM chats WHERE id=?1", [&chat.id], |row| {
        row.get(0)
    })?;
    // Checked with the record, so a removal that began while the start
    // waited finds the chat below the parent or refuses it.
    if let Some((parent, removing)) = &parent {
        ensure!(!removing.reserved(parent), "{parent} is being removed");
        workspace::ensure_parent(&tx, &chat.repository_id, &chat.id, parent)?;
        workspace::set_parent(&tx, &chat.id, Some(parent))?;
        chat.parent = workspace::parent(&tx, &chat.id)?;
    }
    tx.commit()?;
    Ok(chat)
}

/// The scope token of the chat `id`, which its agent carries.
pub(crate) fn scope_token(db: &Connection, id: &str) -> Result<Option<String>> {
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
    repository_id: &str,
    workspace: Option<&str>,
) -> Result<()> {
    ensure!(
        !store::repository_removing(tx, repository_id)?,
        "the repository is being removed"
    );
    // A chat edits the checkout, which a move may be copying.
    store::ensure_repository_not_moving(tx, repository_id)?;
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

/// `base`, or `base-<suffix>` from 2 on, shortened to fit the name limit.
pub(crate) fn suffixed(base: &str, suffix: u64) -> String {
    let suffix = match suffix {
        1 => String::new(),
        n => format!("-{n}"),
    };
    let kept = crate::validate::MAX_NAME_LEN.saturating_sub(suffix.len());
    let mut name: String = base.chars().take(kept).collect();
    name.push_str(&suffix);
    name
}

/// Rename the running or stopped chat `chat`, by ID or name, to `name` unless
/// another holds it. Returns the chat before and after.
fn rename(db: &mut Connection, chat: &str, name: &str) -> Result<(Chat, Chat)> {
    let tx = db.transaction()?;
    let old = recorded(&tx)?
        .into_iter()
        .find(|running| running.id == chat || running.name == chat)
        .with_context(|| format!("no running chat {chat}"))?;
    if old.name != name {
        // The starting chat's files are not open yet under its name.
        ensure!(
            !store::exists(
                &tx,
                "SELECT 1 FROM chats WHERE id=?1 AND starting",
                [&old.id]
            )?,
            "chat {} is still starting; rename it once it runs",
            old.name
        );
        store::ensure_name_available(&tx, name, None)?;
        tx.execute(
            "UPDATE chats SET name=?2 WHERE id=?1",
            params![old.id, name],
        )?;
    }
    tx.commit()?;
    let renamed = Chat {
        name: name.to_owned(),
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
fn move_to(db: &mut Connection, chat: &str, process: &Identity) -> Result<Chat> {
    let moved = db.execute(
        "UPDATE chats SET pid=?2,birth=?3,unique_id=?4,starting=0 WHERE id=?1 AND restore IS NULL",
        params![chat, process.pid, process.birth, unique_id(process)?],
    )?;
    anyhow::ensure!(moved == 1, "no running chat {chat}");
    recorded(db)?
        .into_iter()
        .find(|running| running.id == chat)
        .context("the chat ended")
}

/// The running and stopped chats in start order.
fn recorded(db: &Connection) -> Result<Vec<Chat>> {
    db.prepare(
        "SELECT chats.id,chats.repository_id,chats.name,chats.agent,chats.pid,
                    chats.started_at,chats.origin,repositories.workspaces_dir,chats.session,
                    chats.restore,chats.completion,
                    workspaces.id,workspaces.name,workspaces.branch,chats.number
             FROM chats JOIN repositories ON repositories.id=chats.repository_id
                LEFT JOIN workspaces ON workspaces.id=chats.workspace_id
             ORDER BY chats.started_at, chats.rowid",
    )?
    .query_map([], |row| {
        let id: String = row.get(0)?;
        let repository_id: String = row.get(1)?;
        let tmp_dir = row.get::<_, Option<String>>(7)?.map(|directory| {
            tmp_dirs::Layout::new(Path::new(&directory), &repository_id).locate(&id)
        });
        Ok(Chat {
            id,
            number: row.get(14)?,
            repository_id,
            name: row.get(2)?,
            agent: row.get(3)?,
            workspace: match row.get::<_, Option<String>>(11)? {
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
            session: row.get(8)?,
            restore: row.get(9)?,
            links: SpaceLinks::default(),
            review: Vec::new(),
            completion: store::json_column(row, "completion")?,
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
        Ok(chat)
    })
    .collect()
}

/// The process the chat `id` runs as, unless it is stopped.
fn recorded_process(db: &mut Connection, id: &str) -> Result<Option<Identity>> {
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
fn recorded_processes(db: &Connection) -> Result<Vec<(String, Identity)>> {
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
fn finished(recorded: Vec<(String, Identity)>) -> Vec<(String, Identity)> {
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
fn stop(db: &mut Connection, exited: &[(String, Identity)]) -> Result<Vec<ApprovalRequest>> {
    let tx = db.transaction()?;
    let mut released = Vec::new();
    for (id, process) in exited {
        let stopped = tx.execute(
            "UPDATE chats SET restore=?4,starting=0,stopped_at=unixepoch(),saved=0
             WHERE id=?1 AND pid=?2 AND birth=?3 AND restore IS NULL",
            params![id, process.pid, process.birth, ChatRestore::Resume],
        )?;
        if stopped == 1 {
            released.extend(release_decided_by(&tx, id)?);
        }
    }
    tx.commit()?;
    Ok(released)
}

/// Delete the record of the chat `id`, just started as `process`, which
/// never ran.
fn forget_unstarted(db: &mut Connection, id: &str, process: &Identity) -> Result<()> {
    db.execute(
        "DELETE FROM chats WHERE id=?1 AND pid=?2 AND birth=?3 AND restore IS NULL",
        params![id, process.pid, process.birth],
    )?;
    Ok(())
}

mod done;
mod processes;
mod removal;
mod removing;
mod restores;
mod saved;
mod stopped;
mod stopping;

pub(crate) use removing::Removing;
pub(crate) use stopping::Stopping;

#[cfg(test)]
mod tests;
