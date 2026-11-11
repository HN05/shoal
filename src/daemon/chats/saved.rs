//! Stopped chats: those Shoal stops with their sessions saved, as a
//! shutdown, a logout or `shoal daemon stop` stops a workspace's agent, and
//! those whose agent exited. A stopped chat keeps its record, and with it its
//! name, files, messages, workspaces and temporary directory, until a start
//! restores it in a new process or it is removed.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, ensure};
use futures_util::future::join_all;
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    ensure_place_free,
    processes::{self, ChatProcesses},
    recorded, stopped,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{
        approvals::{ApprovalRequest, release_decided_by},
        log, store,
        workspace::Manager,
    },
    model::{Chat, Repository, StoppedChat},
    process::identity::{self, Identity},
    sessions::Origin,
    state::ChatRestore,
};

impl Manager {
    /// Save every running chat to come back as `restore` says, then stop the
    /// processes each owns, so none goes on writing its session. A chat
    /// whose processes do not stop runs on.
    pub(crate) async fn save_chats(&self, restore: ChatRestore) {
        let _saving = self.chat_saves.lock().await;
        // A handoff would replace the process the stop below ends.
        let _handoffs = self.chat_handoffs.write().await;
        // A chat whose agent exited since the last sweep waits for
        // `shoal resume`, and would otherwise come back on its own.
        if let Err(error) = self.stop_finished_chats().await {
            log!("exited chats not found before saving: {error:#}");
        }
        let Saved {
            running: saved,
            released,
        } = match self
            .store
            .write(move |tx| save(tx, restore, None, None))
            .await
        {
            Ok(saved) => saved,
            Err(error) => {
                log!("chats not saved: {error:#}");
                return;
            }
        };
        let stops = saved
            .iter()
            .map(|(id, process)| self.stop_saved(id, process));
        let stopped = join_all(stops).await;
        for ((id, process), stopped) in saved.into_iter().zip(stopped) {
            if let Err(error) = stopped {
                log!("chat {id} not fully stopped: {error:#}");
                self.keep_after_failed_stop(id, process).await;
            }
        }
        self.ask_user_about(&released).await;
    }

    /// Save the running chat `id` for `shoal resume`, then stop the processes
    /// it owns, as [`Self::save_chats`] saves each chat; the process of the
    /// space `stopped_by` stops it, which may resume it.
    pub(crate) async fn save_chat(&self, id: &SpaceId, stopped_by: Option<&SpaceId>) -> Result<()> {
        let saved = self
            .save_running_chat(id, stopped_by, Saving::Restart)
            .await?;
        ensure!(saved, "the chat no longer runs");
        Ok(())
    }

    /// Save `chat` for `shoal stop` as [`Self::save_chat`] does, and keep
    /// one whose automatic restore is pending from coming back on its own,
    /// both stopped by the space `stopped_by`; any other stopped chat keeps
    /// the space recorded as its stopper.
    pub(crate) async fn stop_chat_for_resume(
        &self,
        chat: &Chat,
        stopped_by: Option<&SpaceId>,
    ) -> Result<StoppedChat> {
        let saving = Saving::Stop(chat);
        let saved = self.save_running_chat(&chat.id, stopped_by, saving).await?;
        Ok(StoppedChat {
            already_stopped: !saved,
        })
    }

    /// Refuse a `shoal stop` of `chat` while a restart, or a restore the
    /// daemon claimed on its own, starts it again, which would bring it back.
    pub(crate) fn check_chat_stop(&self, chat: &Chat) -> Result<()> {
        ensure!(
            !self.chat_agents.starting_again(&chat.id),
            "chat {} starts again now; stop it once it runs",
            chat.name
        );
        Ok(())
    }

    /// The chats `shoal stop --all` stops: those that run, come back on
    /// their own, start again now, whose stop is refused, or have processes
    /// left running since they stopped.
    pub(crate) async fn chat_stop_targets(&self) -> Result<Vec<Chat>> {
        let mut targets = Vec::new();
        for chat in self.chats().await? {
            let target = matches!(chat.restore, None | Some(ChatRestore::Automatic))
                || self.chat_agents.starting_again(&chat.id)
                || !self.chat_survivors(&chat).await?.is_empty();
            if target {
                targets.push(chat);
            }
        }
        Ok(targets)
    }

    /// [`Self::save_chat`] for `saving`, with no other save, handoff or
    /// automatic restore under way, returning whether the chat ran or its
    /// automatic restore was pending. A stop also ends what a stopped chat
    /// owns that still runs.
    async fn save_running_chat(
        &self,
        id: &SpaceId,
        stopped_by: Option<&SpaceId>,
        saving: Saving<'_>,
    ) -> Result<bool> {
        let _saving = self.chat_saves.lock().await;
        let _handoffs = self.chat_handoffs.write().await;
        let stop = matches!(saving, Saving::Stop(_));
        if let Saving::Stop(chat) = saving {
            self.check_chat_stop(chat)?;
        }
        self.stop_finished_chats().await?;
        let chat = id.to_owned();
        let stopped_by = stopped_by.cloned();
        let (saved, stopping, released) = (self.store)
            .write(move |tx| {
                let held = stop && hold_automatic(tx, &chat, stopped_by.as_ref())?;
                let Saved { running, released } =
                    save(tx, ChatRestore::Resume, Some(&chat), stopped_by.as_ref())?;
                let saved = held || !running.is_empty();
                // A save an interrupted daemon never stopped, or a stop that
                // failed, may leave what a stopped chat owns running.
                let stopping = match running.into_iter().next() {
                    Some(running) => Some(running),
                    None if stop => stopped_process(tx, &chat)?.map(|process| (chat, process)),
                    None => None,
                };
                Ok((saved, stopping, released))
            })
            .await?;
        let Some((id, process)) = stopping else {
            return Ok(saved);
        };
        let stopped = self.stop_saved(&id, &process).await;
        if stopped.is_err() {
            self.keep_after_failed_stop(id, process).await;
        }
        self.ask_user_about(&released).await;
        stopped.map(|()| saved)
    }

    /// Settle the chat `id`, saved while it ran as `process`, after a stop
    /// that failed: it runs on while that process lives, and otherwise stays
    /// saved to come back as the save said. A restore or removal waits for
    /// what the stop left running.
    async fn keep_after_failed_stop(&self, id: SpaceId, process: Identity) {
        match identity::alive(&process) {
            Ok(true) => {
                let chat = id.clone();
                if let Err(error) = self.store.run(move |db| unsave(db, &chat, &process)).await {
                    log!("chat {id} stays saved: {error:#}");
                }
            }
            Ok(false) => log!("chat {id} stays saved; its agent ended"),
            Err(error) => log!("chat {id} stays saved: {error:#}"),
        }
    }

    /// Stop the processes of the chat `id`, saved as running as `process`:
    /// those it owns while that process runs, otherwise those it is proven
    /// to have left running. Members of the group it led prove no ownership
    /// once it ended, so they only hold its restore.
    async fn stop_saved(&self, id: &SpaceId, process: &Identity) -> Result<()> {
        let wrappers = self.execution_wrappers().await?;
        let stopped = self.stopped_chat_processes(id).await?;
        let record = |found| self.record_chat_stopping(id, found);
        if let Some(processes) = ChatProcesses::of(id, process, wrappers.clone())? {
            return processes.stop(stopped, record).await;
        }
        let chat = id.to_owned();
        let recorded = self.store.run(move |db| saved_process(db, &chat)).await?;
        processes::stop_owned(id, &recorded, stopped, &wrappers, record).await
    }

    /// Give each saved chat its agent's permissions, which its restore runs
    /// under, all or none: a chat restored or removed since refuses them.
    pub(crate) async fn give_saved_chats(
        &self,
        given: Vec<(SpaceId, PermissionsConfig)>,
    ) -> Result<()> {
        let given = (given.into_iter())
            .map(|(id, given)| Ok((id, store::narrowing_column(&given)?)))
            .collect::<Result<Vec<_>>>()?;
        self.store
            .write(move |tx| {
                for (id, narrowing) in given {
                    let updated = tx.execute(
                        "UPDATE chats SET narrowing=?2 WHERE id=?1 AND restore IS NOT NULL",
                        params![id, narrowing],
                    )?;
                    ensure!(updated == 1, "chat {id} is no longer saved");
                }
                Ok(())
            })
            .await
    }

    /// Run the stopped chat `id` again as process `pid` under `narrowing`,
    /// which replaces the one it recorded, at `origin` in a terminal host
    /// when it runs in one, or when `hands_over`, in a session `pid` starts
    /// and [`Self::move_chat`] names, as [`Self::start_chat`] records a new
    /// chat. It keeps its ID, name, agent and start time.
    pub async fn restore_chat(
        &self,
        id: &SpaceId,
        pid: u32,
        narrowing: PermissionsConfig,
        origin: Option<Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let _saving = self.chat_saves.lock().await;
        let saved = self.saved_chat(id).await?;
        ensure!(
            !self.chat_removals.reserved(&saved.id),
            "chat {} is being removed",
            saved.name
        );
        self.run_again(&saved, pid, narrowing, origin, hands_over)
            .await
    }

    /// [`Self::restore_chat`] for the `saved` chat, once a save under way
    /// finished.
    async fn run_again(
        &self,
        saved: &Chat,
        pid: u32,
        narrowing: PermissionsConfig,
        origin: Option<Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        // A shutdown's save is over or under way, which would end the
        // restored agent with nothing left to bring it back.
        ensure!(
            !self.agent_queue.suspended(),
            "Shoal is stopping work for a shutdown or a daemon stop"
        );
        self.ensure_ended(saved, pid).await?;
        if let Some(workspace) = &saved.workspace {
            self.chat_workspace(&SpaceSelector::id(&workspace.id))
                .await?;
        }
        let repository = match &saved.repository_id {
            Some(id) => Some(self.reserve_repository_directory(id).await?),
            None => None,
        };
        let restored = Restored {
            id: saved.id.clone(),
            repository: saved.repository_id.clone(),
            workspace: saved
                .workspace
                .as_ref()
                .map(|workspace| workspace.id.clone()),
            process,
            narrowing,
            origin,
            hands_over,
        };
        let owner = self.root_dir_owner();
        let chat = (self.store)
            .write(move |tx| restore(tx, &owner, &restored))
            .await?;
        let chat = (self.give_restored_tmp_dir(chat, repository.as_ref(), pid)).await?;
        self.take_chat_place(&chat).await?;
        Ok(chat)
    }

    /// `chat`, just restored as process `pid`, with its temporary directory,
    /// created again when missing; a chat that cannot have one waits for
    /// `shoal resume` again rather than ending.
    async fn give_restored_tmp_dir(
        &self,
        mut chat: Chat,
        repository: Option<&Repository>,
        pid: u32,
    ) -> Result<Chat> {
        match self.chat_tmp_dir(repository, &chat) {
            Ok(tmp_dir) => {
                chat.tmp_dir = tmp_dir;
                Ok(chat)
            }
            Err(error) => {
                let id = chat.id.clone();
                let owner = self.root_dir_owner();
                let (_, released) = (self.store)
                    .write(move |tx| resave(tx, &owner, &id, pid, None))
                    .await?;
                self.ask_user_about(&released).await;
                Err(error)
            }
        }
    }

    /// Record that process `pid` could not restore the chat `id`, failing
    /// with `error`: the chat waits for `shoal resume` again, if that
    /// process had taken it over, so the end of the process does not end it,
    /// and the user, who may not see where it ran, learns why.
    ///
    /// The space `stopped_by`, whose process started `pid`, stopped the chat;
    /// else the one whose restart of it is in progress, since the daemon's
    /// own `shoal resume` for that restart has no scope.
    pub async fn return_to_saved(
        &self,
        id: &SpaceId,
        pid: u32,
        error: &str,
        stopped_by: Option<&SpaceId>,
    ) -> Result<()> {
        let chat = id.to_owned();
        let owner = self.root_dir_owner();
        let stopped_by = stopped_by.cloned().or_else(|| self.chat_agents.stopper(id));
        let (saved, released) = (self.store)
            .write(move |tx| resave(tx, &owner, &chat, pid, stopped_by.as_ref()))
            .await?;
        self.ask_user_about(&released).await;
        self.notify_restore_failed(&saved, error).await;
        Ok(())
    }

    /// The space whose process stopped the stopped chat `id`; none when the
    /// user or Shoal stopped it, or its agent exited.
    pub(crate) async fn chat_stopped_by(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| {
                let stopper = db
                    .query_row(
                        "SELECT stopped_by FROM chats WHERE id=?1 AND restore IS NOT NULL",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(stopper.flatten())
            })
            .await
    }

    /// The stopped chat `id`.
    async fn saved_chat(&self, id: &SpaceId) -> Result<Chat> {
        self.chat_by_id(id)
            .await?
            .filter(|chat| chat.restore.is_some())
            .with_context(|| format!("no stopped chat {id}"))
    }

    /// Refuse the stopped `chat` while a process it owns still runs, other
    /// than `caller`, as after a stop that did not end them: a restore would
    /// have two agents write one session, and a removal would delete files
    /// they use.
    pub(crate) async fn ensure_ended(&self, chat: &Chat, caller: u32) -> Result<()> {
        let mut running = self.chat_survivors(chat).await?;
        running.retain(|process| process.pid != caller);
        let pids: Vec<String> = running
            .iter()
            .map(|process| process.pid.to_string())
            .collect();
        ensure!(
            pids.is_empty(),
            "chat {} still has processes running; end them with kill {}",
            chat.name,
            pids.join(" ")
        );
        Ok(())
    }

    /// The processes the stopped `chat` owns that still run, as after a
    /// stop that did not end them.
    async fn chat_survivors(&self, chat: &Chat) -> Result<Vec<Identity>> {
        if chat.restore.is_none() {
            return Ok(Vec::new());
        }
        let id = chat.id.clone();
        let recorded = self.store.run(move |db| saved_process(db, &id)).await?;
        let wrappers = self.execution_wrappers().await?;
        let stopped = self.stopped_chat_processes(&chat.id).await?;
        processes::survivors(&chat.id, &recorded, &stopped, &wrappers).await
    }
}

/// The process the chat `id` ran as while it is stopped.
fn stopped_process(db: &Connection, id: &SpaceId) -> Result<Option<Identity>> {
    let stopped = db
        .query_row(
            "SELECT 1 FROM chats WHERE id=?1 AND restore IS NOT NULL",
            [id],
            |_| Ok(()),
        )
        .optional()?;
    stopped.map(|()| saved_process(db, id)).transpose()
}

/// The process the stopped chat `id` ran as.
fn saved_process(db: &Connection, id: &SpaceId) -> Result<Identity> {
    let (pid, birth, unique): (u32, String, Option<String>) = db.query_row(
        "SELECT pid,birth,unique_id FROM chats WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(Identity {
        pid,
        birth,
        unique: unique.as_deref().map(serde_json::from_str).transpose()?,
    })
}

/// Why [`Manager::save_running_chat`] saves a chat.
enum Saving<'a> {
    /// A restart, which starts it again at once.
    Restart,
    /// `shoal stop` of the chat, which stays down until `shoal resume`.
    Stop(&'a Chat),
}

/// Turn the pending automatic restore of the chat `id` into a stop by the
/// process of the space `stopped_by`, which waits for `shoal resume`,
/// returning whether one was pending.
fn hold_automatic(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    stopped_by: Option<&SpaceId>,
) -> Result<bool> {
    let held = tx.execute(
        "UPDATE chats SET restore=?2,stopped_by=?3 WHERE id=?1 AND restore=?4",
        params![id, ChatRestore::Resume, stopped_by, ChatRestore::Automatic],
    )?;
    Ok(held == 1)
}

/// What [`save`] saved.
struct Saved {
    /// The ID and process of each chat.
    running: Vec<(SpaceId, Identity)>,
    /// The requests they decided, given back to the user, since a saved
    /// chat decides none.
    released: Vec<ApprovalRequest>,
}

/// Mark every running chat, or only the chat `only`, saved to come back as
/// `restore` says, stopped by the process of the space `stopped_by`, or by
/// Shoal.
fn save(
    tx: &rusqlite::Transaction<'_>,
    restore: ChatRestore,
    only: Option<&SpaceId>,
    stopped_by: Option<&SpaceId>,
) -> Result<Saved> {
    let mut running = super::recorded_processes(tx)?;
    running.retain(|(id, _)| only.is_none_or(|only| only == id));
    let mut released = Vec::new();
    for (id, process) in &running {
        tx.execute(
            "UPDATE chats SET restore=?2,unique_id=COALESCE(?3,unique_id),stopped_at=unixepoch(),
                saved=1,stopped_by=?4 WHERE id=?1",
            params![id, restore, live_unique_id(process)?, stopped_by],
        )?;
        released.extend(release_decided_by(tx, id)?);
    }
    Ok(Saved { running, released })
}

/// The never-reused ID of `process`, as JSON, while it runs.
fn live_unique_id(process: &Identity) -> Result<Option<String>> {
    let unique = identity::capture(process.pid)?
        .filter(|live| live == process)
        .and_then(|live| live.unique);
    Ok(unique
        .map(|unique| serde_json::to_string(&unique))
        .transpose()?)
}

/// Mark the chat `id` saved for `shoal resume` while it runs as process
/// `pid`, stopped by the process of the space `stopped_by`, and refuse it
/// once it runs as another, returning it with the requests it decided, given
/// back to the user. A chat `pid` never took over keeps its stopper.
fn resave(
    tx: &rusqlite::Transaction<'_>,
    owner: &std::path::Path,
    id: &SpaceId,
    pid: u32,
    stopped_by: Option<&SpaceId>,
) -> Result<(Chat, Vec<ApprovalRequest>)> {
    tx.execute(
        "UPDATE chats SET restore=?3,stopped_at=unixepoch(),saved=1
         WHERE id=?1 AND pid=?2 AND restore IS NULL",
        params![id, pid, ChatRestore::Resume],
    )?;
    // Also once a failure inside the restore saved it again.
    tx.execute(
        "UPDATE chats SET stopped_by=?3 WHERE id=?1 AND pid=?2",
        params![id, pid, stopped_by],
    )?;
    let chat = recorded(tx, owner)?
        .into_iter()
        .find(|chat| chat.id == *id)
        .context("the chat ended")?;
    ensure!(
        chat.restore.is_some(),
        "chat {id} runs as process {}",
        chat.pid
    );
    let released = release_decided_by(tx, id)?;
    Ok((chat, released))
}

/// Return the chat `id`, saved while it ran as `process`, to running.
fn unsave(db: &mut Connection, id: &SpaceId, process: &Identity) -> Result<()> {
    db.execute(
        "UPDATE chats SET restore=NULL,stopped_at=NULL,saved=0
         WHERE id=?1 AND pid=?2 AND birth=?3",
        params![id, process.pid, process.birth],
    )?;
    Ok(())
}

/// Whether the process the stopped `chat` ran as still runs, as one a stop
/// left running.
pub(super) fn still_runs(db: &Connection, chat: &Chat) -> Result<bool> {
    let birth: String = db.query_row("SELECT birth FROM chats WHERE id=?1", [&chat.id], |row| {
        row.get(0)
    })?;
    identity::alive(&Identity {
        pid: chat.pid,
        birth,
        unique: None,
    })
}

/// A stopped chat that runs again.
struct Restored {
    id: SpaceId,
    repository: Option<RepositoryId>,
    /// The workspace whose worktree it runs in.
    workspace: Option<SpaceId>,
    /// The process it runs as, or that hands it to a session it starts.
    process: Identity,
    /// The `--permission` narrowing it runs under.
    narrowing: PermissionsConfig,
    origin: Option<Origin>,
    hands_over: bool,
}

/// Record that the stopped chat `restored` names runs again, starting until it
/// is moved when it hands over, unless a restore took it first. The check
/// before it found nothing its stops signalled still running, nor anything
/// they started.
fn restore(
    tx: &rusqlite::Transaction<'_>,
    owner: &std::path::Path,
    restored: &Restored,
) -> Result<Chat> {
    let repository = restored.repository.as_ref();
    ensure_place_free(tx, repository, restored.workspace.as_ref())?;
    let updated = tx.execute(
        "UPDATE chats SET pid=?2,birth=?3,origin=?4,starting=?5,restore=NULL,stopped_at=NULL,saved=0,
            unique_id=?6,stopped_by=NULL,narrowing=?7
         WHERE id=?1 AND restore IS NOT NULL",
        params![
            restored.id,
            restored.process.pid,
            restored.process.birth,
            restored
                .origin
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            restored.hands_over,
            super::unique_id(&restored.process)?,
            store::narrowing_column(&restored.narrowing)?
        ],
    )?;
    ensure!(updated == 1, "the chat is no longer stopped");
    stopped::forget(tx, &restored.id)?;
    let chat = recorded(tx, owner)?
        .into_iter()
        .find(|chat| chat.id == restored.id)
        .context("the chat ended")?;
    Ok(chat)
}

#[cfg(test)]
pub(super) mod tests;
