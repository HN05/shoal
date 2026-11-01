//! Chats Shoal stops with their sessions saved, as a shutdown, a logout or
//! `shoal daemon stop` stops a workspace's agent. A saved chat keeps its
//! record, and with it its name, files, messages, workspaces and temporary
//! directory, until a start restores it in a new process or it is discarded.
use anyhow::{Context as _, Result, ensure};
use futures_util::future::join_all;
use rusqlite::{Connection, params};

use super::{
    Over, ensure_checkout_free,
    processes::{self, ChatProcesses},
    recorded,
};
use crate::{
    daemon::{log, workspace::Manager},
    model::Chat,
    process::identity::{self, Identity},
    sessions::Origin,
    state::ChatRestore,
    tmp_dirs,
};

impl Manager {
    /// Save every running chat to come back as `restore` says, then stop the
    /// processes each owns, so none goes on writing its session. A chat
    /// whose processes do not stop runs on.
    pub(crate) async fn save_chats(&self, restore: ChatRestore) {
        let _saving = self.chat_saves.lock().await;
        let saved = match self.store.run(move |db| save(db, restore)).await {
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
    }

    /// Settle the chat `id`, saved while it ran as `process`, after a stop
    /// that failed: it runs on while that process lives, and otherwise stays
    /// saved, since running with its process ended would end it, deleting
    /// its temporary directory. A restore or discard waits for what the stop
    /// left running.
    async fn keep_after_failed_stop(&self, id: String, process: Identity) {
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

    /// Stop the processes of the chat `id`, saved as running as `process`.
    async fn stop_saved(&self, id: &str, process: &Identity) -> Result<()> {
        let wrappers = self.execution_wrappers().await?;
        match ChatProcesses::of(id, process, wrappers)? {
            Some(processes) => processes.stop().await,
            None => Ok(()),
        }
    }

    /// Run the saved chat `id` again as process `pid`, at `origin` in a
    /// terminal host when it runs in one, or when `hands_over`, in a session
    /// `pid` starts and [`Self::move_chat`] names, as
    /// [`Self::start_chat`] records a new chat. It keeps its ID, name,
    /// agent and start time.
    pub async fn restore_chat(
        &self,
        id: &str,
        pid: u32,
        origin: Option<Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let _saving = self.chat_saves.lock().await;
        let saved = self.saved_chat(id).await?;
        self.ensure_ended(&saved, pid).await?;
        let repository = self
            .reserve_repository_directory(&saved.repository_id)
            .await?;
        if let Some(origin) = &origin {
            self.release_place(origin).await?;
        }
        let restored = Restored {
            id: saved.id,
            repository: saved.repository_id,
            process: process.clone(),
            origin,
            hands_over,
        };
        let mut chat = self.store.run(move |db| restore(db, &restored)).await?;
        match tmp_dirs::directory(&repository, &chat.id) {
            Ok(tmp_dir) => {
                chat.tmp_dir = tmp_dir;
                Ok(chat)
            }
            Err(error) => {
                self.return_to_saved(&chat.id, pid, &format!("{error:#}"))
                    .await?;
                Err(error)
            }
        }
    }

    /// Return the chat `id`, restored as process `pid`, to waiting for
    /// `shoal resume`, as when its restore failed with `error` before its
    /// agent ran, so the end of that process does not end it, and tell the
    /// user, who may not see where it ran.
    pub async fn return_to_saved(&self, id: &str, pid: u32, error: &str) -> Result<()> {
        let chat = id.to_owned();
        let saved = self.store.run(move |db| resave(db, &chat, pid)).await?;
        self.notify_restore_failed(&saved, error).await;
        Ok(())
    }

    /// End the saved chat `selector` names, by ID or name, without
    /// restoring it, as if its process had exited.
    pub(crate) async fn discard_saved_chat(&self, selector: &str) -> Result<Chat> {
        let _saving = self.chat_saves.lock().await;
        let chat = self.saved_chat(selector).await?;
        self.ensure_ended(&chat, std::process::id()).await?;
        self.end_chats(vec![(chat.id.clone(), Over::Discarded)])
            .await?;
        Ok(chat)
    }

    /// The saved chat `selector` names, by ID or name.
    async fn saved_chat(&self, selector: &str) -> Result<Chat> {
        self.recorded_chat(selector)
            .await?
            .filter(|chat| chat.restore.is_some())
            .with_context(|| format!("no saved chat {selector}"))
    }

    /// Refuse the saved `chat` while a process it owns still runs, other
    /// than `caller`, as after a stop that did not end them: a restore would
    /// have two agents write one session, and a discard would delete files
    /// they use.
    async fn ensure_ended(&self, chat: &Chat, caller: u32) -> Result<()> {
        let id = chat.id.clone();
        let recorded = self.store.run(move |db| saved_process(db, &id)).await?;
        let wrappers = self.execution_wrappers().await?;
        let mut running = processes::survivors(&chat.id, &recorded, &wrappers).await?;
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
}

/// The process the saved chat `id` ran as.
fn saved_process(db: &mut Connection, id: &str) -> Result<Identity> {
    Ok(
        db.query_row("SELECT pid,birth FROM chats WHERE id=?1", [id], |row| {
            Ok(Identity {
                pid: row.get(0)?,
                birth: row.get(1)?,
                unique: None,
            })
        })?,
    )
}

/// Mark every running chat saved to come back as `restore` says, returning
/// the ID and process of each.
fn save(db: &mut Connection, restore: ChatRestore) -> Result<Vec<(String, Identity)>> {
    let tx = db.transaction()?;
    let running = super::recorded_processes(&tx)?;
    tx.execute(
        "UPDATE chats SET restore=?1 WHERE restore IS NULL",
        [restore],
    )?;
    tx.commit()?;
    Ok(running)
}

/// Mark the running chat `id` of process `pid` saved for `shoal resume`,
/// returning it.
fn resave(db: &mut Connection, id: &str, pid: u32) -> Result<Chat> {
    let tx = db.transaction()?;
    let saved = tx.execute(
        "UPDATE chats SET restore=?3 WHERE id=?1 AND pid=?2 AND restore IS NULL",
        params![id, pid, ChatRestore::Resume],
    )?;
    ensure!(saved == 1, "chat {id} does not run as process {pid}");
    let chat = recorded(&tx)?
        .into_iter()
        .find(|chat| chat.id == id)
        .context("the chat ended")?;
    tx.commit()?;
    Ok(chat)
}

/// Return the chat `id`, saved while it ran as `process`, to running.
fn unsave(db: &mut Connection, id: &str, process: &Identity) -> Result<()> {
    db.execute(
        "UPDATE chats SET restore=NULL WHERE id=?1 AND pid=?2 AND birth=?3",
        params![id, process.pid, process.birth],
    )?;
    Ok(())
}

/// Whether the process the saved `chat` ran as still runs, as one a stop
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

/// A saved chat that runs again.
struct Restored {
    id: String,
    repository: String,
    /// The process it runs as, or that hands it to a session it starts.
    process: Identity,
    origin: Option<Origin>,
    hands_over: bool,
}

/// Record that the saved chat `restored` names runs again, starting until it
/// is moved when it hands over, unless a restore took it first.
fn restore(db: &mut Connection, restored: &Restored) -> Result<Chat> {
    let tx = db.transaction()?;
    ensure_checkout_free(&tx, &restored.repository)?;
    let updated = tx.execute(
        "UPDATE chats SET pid=?2,birth=?3,origin=?4,starting=?5,restore=NULL
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
            restored.hands_over
        ],
    )?;
    ensure!(updated == 1, "the chat is no longer saved");
    let chat = recorded(&tx)?
        .into_iter()
        .find(|chat| chat.id == restored.id)
        .context("the chat ended")?;
    tx.commit()?;
    Ok(chat)
}

#[cfg(test)]
pub(super) mod tests;
