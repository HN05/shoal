//! Chats Shoal stops with their sessions saved, as a shutdown, a logout or
//! `shoal daemon stop` stops a workspace's agent. A saved chat keeps its
//! record, and with it its name, files, messages, workspaces and temporary
//! directory.
use anyhow::Result;
use futures_util::future::join_all;
use rusqlite::{Connection, params};

use super::processes::ChatProcesses;
use crate::{
    daemon::{log, workspace::Manager},
    process::identity::Identity,
    state::ChatRestore,
};

impl Manager {
    /// Save every running chat to come back as `restore` says, then stop the
    /// processes each owns, so none goes on writing its session. A chat
    /// whose processes do not stop runs on.
    pub(crate) async fn save_chats(&self, restore: ChatRestore) {
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
                log!("chat {id} not stopped, so it runs on: {error:#}");
                let chat = id.clone();
                if let Err(error) = self.store.run(move |db| unsave(db, &chat, &process)).await {
                    log!("chat {id} stays saved: {error:#}");
                }
            }
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

/// Return the chat `id`, saved while it ran as `process`, to running.
fn unsave(db: &mut Connection, id: &str, process: &Identity) -> Result<()> {
    db.execute(
        "UPDATE chats SET restore=NULL WHERE id=?1 AND pid=?2 AND birth=?3",
        params![id, process.pid, process.birth],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
