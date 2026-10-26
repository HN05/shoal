//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::daemon::workspace::Manager;

/// Unread messages one workspace may hold, and their total size; together
/// they fit one response.
pub(crate) const MAX_UNREAD: i64 = 50;
pub(crate) const MAX_UNREAD_BYTES: usize = 50 * crate::validate::MAX_MESSAGE_LEN;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    pub message: String,
}

/// Queue `message` in the caller's transaction, returning its ID, or `None`
/// when an identical message is still unread. Callers wake watches with
/// `agent_messages_changed` after committing.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    workspace_id: &str,
    message: &str,
) -> Result<Option<i64>> {
    let pending: Option<i64> = tx
        .query_row(
            "SELECT id FROM agent_messages WHERE workspace_id=?1 AND message=?2",
            params![workspace_id, message],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(None);
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(LENGTH(CAST(message AS BLOB))),0)
            FROM agent_messages WHERE workspace_id=?1",
        [workspace_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the workspace already has {MAX_UNREAD} unread agent messages"
    );
    ensure!(
        usize::try_from(bytes)? + message.len() <= MAX_UNREAD_BYTES,
        "the workspace's unread agent messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO agent_messages(workspace_id,created_at,message) VALUES (?1,?2,?3)",
        params![
            workspace_id,
            i64::try_from(crate::time::unix_seconds())?,
            message
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

impl Manager {
    /// Queue a message for the workspace's agents. An identical unread message
    /// is not repeated.
    pub async fn send_agent_message(&self, selector: &str, message: String) -> Result<()> {
        crate::validate::agent_message(&message)?;
        let workspace = self.workspace(selector).await?;
        self.queue_agent_message(&workspace.id, message).await
    }

    pub(crate) async fn queue_agent_message(
        &self,
        workspace_id: &str,
        message: String,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let queued = insert(&tx, &workspace_id, &message)?;
                tx.commit()?;
                Ok(queued.is_some())
            })
            .await?;
        if queued {
            self.agent_messages_changed.send_modify(|count| *count += 1);
        }
        Ok(())
    }

    /// Unread messages, oldest first. They stay unread until delivered.
    pub async fn agent_messages(&self, selector: &str) -> Result<Vec<AgentMessage>> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,created_at,message FROM agent_messages WHERE workspace_id=?1 ORDER BY id",
                    )?
                    .query_map([workspace.id], |row| {
                        Ok(AgentMessage {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            message: row.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Delete exactly these messages once an agent has received them, with
    /// the questions whose answers they carried.
    pub async fn mark_agent_messages_delivered(&self, selector: &str, ids: Vec<i64>) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in ids {
                    tx.execute(
                        "DELETE FROM agent_messages WHERE id=?1 AND workspace_id=?2",
                        params![id, workspace.id],
                    )?;
                    crate::daemon::questions::delete_delivered(&tx, &workspace.id, id)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.agent_wake_changed.send_modify(|count| *count += 1);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
