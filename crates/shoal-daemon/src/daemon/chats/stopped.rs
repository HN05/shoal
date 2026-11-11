//! The processes a stop of a chat signals, recorded before it signals them,
//! so a later stop and the check before a restore or discard follow the
//! lineage of what they started after an interrupted stop.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{daemon::workspace::Manager, process::identity::Identity};

impl Manager {
    /// The processes earlier stops of the chat `id` signalled.
    pub(super) async fn stopped_chat_processes(&self, id: &SpaceId) -> Result<Vec<Identity>> {
        let chat = id.to_owned();
        self.store.run(move |db| stopped(db, &chat)).await
    }

    /// Record durably that a stop of the chat `id` is about to signal
    /// `processes`.
    pub(super) async fn record_chat_stopping(
        &self,
        id: &SpaceId,
        processes: Vec<Identity>,
    ) -> Result<()> {
        let chat = id.to_owned();
        self.store
            .write(move |tx| record(tx, &chat, &processes))
            .await
    }
}

/// The processes stops of the chat `id` signalled.
fn stopped(db: &mut Connection, id: &SpaceId) -> Result<Vec<Identity>> {
    let mut statement =
        db.prepare("SELECT pid,birth,unique_id FROM chat_stopped_processes WHERE chat_id=?1")?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut processes = Vec::new();
    for row in rows {
        let (pid, birth, unique) = row?;
        processes.push(Identity {
            pid,
            birth,
            unique: unique.as_deref().map(serde_json::from_str).transpose()?,
        });
    }
    Ok(processes)
}

/// Record that a stop of the chat `id` signals `processes`, unless the chat
/// was removed while it stops: then no later stop or check follows them.
fn record(tx: &rusqlite::Transaction<'_>, id: &SpaceId, processes: &[Identity]) -> Result<()> {
    for process in processes {
        tx.execute(
            "INSERT OR IGNORE INTO chat_stopped_processes(chat_id,pid,birth,unique_id)
             SELECT ?1,?2,?3,?4 WHERE EXISTS (SELECT 1 FROM chats WHERE id=?1)",
            params![
                id,
                process.pid,
                process.birth,
                process
                    .unique
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
            ],
        )?;
    }
    Ok(())
}

/// Forget what stops of the chat `id` signalled, once no process it owns
/// runs, as when it is restored.
pub(super) fn forget(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM chat_stopped_processes WHERE chat_id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests;
