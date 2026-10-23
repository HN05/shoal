//! Messages for a chat about the workspaces it coordinates. Like agent
//! messages, delivery deletes them; a chat that ends before receiving them
//! gives them to the user as the notifications they would have been.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::daemon::{
    agent_messages::{AgentMessage, MAX_UNREAD, MAX_UNREAD_BYTES},
    links::wait::Updates,
    notifications::{self, About, NotificationKind},
    workspace::Manager,
};

impl Manager {
    /// Queue `message` about the workspace named `workspace` for the chat
    /// coordinating it. `kind` is the notification the user receives instead
    /// if the chat ends first; without one the message is dropped then. An
    /// identical unread message is not repeated.
    pub(crate) async fn queue_chat_message(
        &self,
        chat: &str,
        workspace: &str,
        workspace_id: Option<&str>,
        kind: Option<NotificationKind>,
        message: String,
    ) -> Result<()> {
        let (chat, workspace) = (chat.to_owned(), workspace.to_owned());
        let workspace_id = workspace_id.map(str::to_owned);
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let about = About {
                    name: Some(workspace),
                    id: workspace_id,
                    subject: None,
                };
                let queued = insert(&tx, &chat, &about, kind, &message)?;
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        if queued {
            self.chat_messages_changed.send_modify(|count| *count += 1);
        }
        Ok(())
    }

    /// The chat's unread messages, oldest first, each naming its workspace.
    pub async fn chat_messages(&self, chat: &str) -> Result<Vec<AgentMessage>> {
        let chat = chat.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,created_at,workspace,message FROM chat_messages
                            WHERE chat_id=?1 ORDER BY id",
                    )?
                    .query_map([chat], |row| {
                        Ok(AgentMessage {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            message: format!(
                                "{}: {}",
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?
                            ),
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// What the chat's agent has not received, without waiting. Delivery
    /// replaces a running wait, which would otherwise deliver them again.
    pub async fn chat_updates(&self, chat: &str) -> Result<Updates> {
        let delivery = self.chat_waits.start(chat);
        let messages = self.chat_messages(chat).await?;
        if !delivery.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates::messages(messages))
    }

    /// The chat's next messages, once it has any, unless a newer wait or
    /// delivery takes them first.
    pub async fn wait_chat_messages(&self, chat: &str) -> Result<Updates> {
        let wait = self.chat_waits.start(chat);
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.chat_messages_changed.subscribe();
        let mut messages = self.chat_messages(chat).await?;
        while messages.is_empty() {
            tokio::select! {
                changed = queued.changed() => changed?,
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
            messages = self.chat_messages(chat).await?;
        }
        if !wait.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates::messages(messages))
    }

    /// Delete exactly these messages once the chat's agent has received them.
    pub async fn mark_chat_messages_delivered(&self, chat: &str, ids: Vec<i64>) -> Result<()> {
        let chat = chat.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in ids {
                    tx.execute(
                        "DELETE FROM chat_messages WHERE id=?1 AND chat_id=?2",
                        params![id, chat],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

/// Queue a message about the named workspace in the caller's transaction,
/// returning whether it was new.
fn insert(
    tx: &Transaction<'_>,
    chat: &str,
    about: &About,
    kind: Option<NotificationKind>,
    message: &str,
) -> Result<bool> {
    let workspace = about.name.as_deref().unwrap_or_default();
    let pending: Option<i64> = tx
        .query_row(
            "SELECT id FROM chat_messages WHERE chat_id=?1 AND workspace=?2 AND message=?3",
            params![chat, workspace, message],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(false);
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(LENGTH(CAST(workspace || message AS BLOB))),0)
            FROM chat_messages WHERE chat_id=?1",
        [chat],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the chat already has {MAX_UNREAD} unread messages"
    );
    ensure!(
        usize::try_from(bytes)? + workspace.len() + message.len() <= MAX_UNREAD_BYTES,
        "the chat's unread messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO chat_messages(chat_id,created_at,workspace,workspace_id,kind,message)
            VALUES (?1,?2,?3,COALESCE(?4,(SELECT id FROM workspaces WHERE name=?3)),?5,?6)",
        params![
            chat,
            i64::try_from(crate::time::unix_seconds())?,
            workspace,
            about.id,
            kind,
            message
        ],
    )?;
    Ok(true)
}

/// Give the user the notifications that the ended chat's unread messages
/// stand for, in the transaction that deletes the chat and with it the
/// messages. Returns the last notification recorded.
pub(super) fn hand_to_user(tx: &Transaction<'_>, chat: &str) -> Result<Option<i64>> {
    let unread = tx
        .prepare(
            "SELECT workspace,workspace_id,kind,message FROM chat_messages
                WHERE chat_id=?1 AND kind IS NOT NULL ORDER BY id",
        )?
        .query_map([chat], |row| {
            let about = About {
                name: Some(row.get(0)?),
                id: row.get(1)?,
                subject: None,
            };
            Ok((about, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(About, NotificationKind, String)>>>()?;
    let mut last = None;
    for (about, kind, message) in unread {
        let recorded = notifications::insert(tx, &about, kind, &message)?;
        if recorded.new {
            last = Some(recorded.id);
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests;
