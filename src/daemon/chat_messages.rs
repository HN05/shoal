//! Messages for a chat about the workspaces it coordinates, and from the
//! user. Like agent messages, delivery deletes them; a chat that ends before
//! receiving them gives them to the user as the notifications they would
//! have been.
use anyhow::{Context as _, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

use std::path::PathBuf;

use crate::daemon::{
    agent_messages::{AgentMessage, MAX_UNREAD, MAX_UNREAD_BYTES},
    links::wait::{RESPONSE_LIMIT, Updates},
    notifications::{self, About, NotificationKind},
    workspace::Manager,
    workspace_files::{self, Attacher, NewFile, layout::Copy},
};

impl Manager {
    /// Queue a message from the user for the running chat `chat` names by ID
    /// or name, with copies of `attachments`, which the chat's attached files
    /// list from then on as attached by `attacher`; without text, the files
    /// are the message. It is dropped if the chat ends first.
    pub async fn send_chat_message(
        &self,
        chat: &str,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let chat = self
            .running_chat(chat)
            .await?
            .with_context(|| format!("no running chat {chat}"))?;
        if attachments.is_empty() {
            return self
                .queue_chat_message(&chat.id, None, None, None, message)
                .await;
        }
        let id = chat.id.clone();
        let text = message.clone();
        self.queue_attached(chat.into(), &text, attachments, |copies| {
            self.queue_attached_chat_message(id, message, copies, attacher.clone())
        })
        .await
    }

    /// Queue `message` from the user with Shoal's copies `attachments`,
    /// which the chat's attached files list from then on as attached by
    /// `attacher`.
    async fn queue_attached_chat_message(
        &self,
        chat: String,
        message: String,
        attachments: Vec<Copy>,
        attacher: Attacher,
    ) -> Result<()> {
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let paths: Vec<_> = attachments.iter().map(|copy| copy.path.clone()).collect();
                let queued = insert(&tx, &chat, &About::default(), None, &message, &paths)?;
                let queued = queued.is_some();
                let source = workspace_files::FileSource::Message;
                workspace_files::record(&tx, &chat, source, Some(&attacher), &attachments)?;
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        if queued {
            self.signals.chat_message_queued();
        }
        Ok(())
    }

    /// Queue `message` for the chat, about the workspace named `workspace`
    /// it coordinates, if any. `kind` is the notification the user receives
    /// instead if the chat ends first; without one the message is dropped
    /// then. An identical unread message is not repeated.
    pub(crate) async fn queue_chat_message(
        &self,
        chat: &str,
        workspace: Option<&str>,
        workspace_id: Option<&str>,
        kind: Option<NotificationKind>,
        message: String,
    ) -> Result<()> {
        let (chat, workspace) = (chat.to_owned(), workspace.map(str::to_owned));
        let workspace_id = workspace_id.map(str::to_owned);
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let about = About {
                    name: workspace,
                    id: workspace_id,
                    subject: None,
                };
                let queued = insert(&tx, &chat, &about, kind, &message, &[])?.is_some();
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        if queued {
            self.signals.chat_message_queued();
        }
        Ok(())
    }

    /// The chat's unread messages, oldest first, each naming its workspace
    /// unless it comes from the user.
    pub async fn chat_messages(&self, chat: &str) -> Result<Vec<AgentMessage>> {
        let chat = chat.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,created_at,workspace,message,attachments FROM chat_messages
                            WHERE chat_id=?1 ORDER BY id",
                    )?
                    .query_map([chat], |row| {
                        let (workspace, message): (String, String) = (row.get(2)?, row.get(3)?);
                        Ok(AgentMessage {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            message: if workspace.is_empty() {
                                message
                            } else {
                                format!("{workspace}: {message}")
                            },
                            attachments: crate::daemon::store::json_column(row, "attachments")?
                                .unwrap_or_default(),
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
        let room = RESPONSE_LIMIT.saturating_sub(serde_json::to_vec(&messages)?.len());
        Ok(Updates {
            leftovers: self.leftovers.undelivered(chat, room)?,
            ..Updates::messages(messages)
        })
    }

    /// The chat's next messages, once it has any, unless a newer wait or
    /// delivery takes them first.
    pub async fn wait_chat_messages(&self, chat: &str) -> Result<Updates> {
        let wait = self.chat_waits.start(chat);
        // Subscribe before reading, so a message queued in between still wakes.
        let mut queued = self.signals.watch_chat_messages();
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

    /// Delete exactly these messages once the chat's agent has received
    /// them, with the questions whose answers they carried.
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
                    crate::daemon::questions::delete_delivered(&tx, &chat, id)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

/// Queue `message` from the user for the chat in the caller's transaction:
/// its ID, or `None` when an identical message is already unread.
pub(crate) fn insert_from_user(
    tx: &Transaction<'_>,
    chat: &str,
    message: &str,
) -> Result<Option<i64>> {
    insert(tx, chat, &About::default(), None, message, &[])
}

/// Queue a message about the named workspace, or from the user without one,
/// with the files `attachments` Shoal has copied, in the caller's
/// transaction: its ID, or `None` when an identical message is already
/// unread.
fn insert(
    tx: &Transaction<'_>,
    chat: &str,
    about: &About,
    kind: Option<NotificationKind>,
    message: &str,
    attachments: &[PathBuf],
) -> Result<Option<i64>> {
    let workspace = about.name.as_deref().unwrap_or_default();
    let attachments = serde_json::to_string(attachments)?;
    let pending: Option<i64> = tx
        .query_row(
            "SELECT id FROM chat_messages
                WHERE chat_id=?1 AND workspace=?2 AND message=?3 AND attachments=?4",
            params![chat, workspace, message, attachments],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(None);
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(LENGTH(CAST(workspace || message || attachments AS BLOB))),0)
            FROM chat_messages WHERE chat_id=?1",
        [chat],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the chat already has {MAX_UNREAD} unread messages"
    );
    ensure!(
        usize::try_from(bytes)? + workspace.len() + message.len() + attachments.len()
            <= MAX_UNREAD_BYTES,
        "the chat's unread messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO chat_messages
            (chat_id,created_at,workspace,workspace_id,kind,message,attachments)
            VALUES (?1,?2,?3,COALESCE(?4,(SELECT id FROM workspaces WHERE name=?3)),?5,?6,?7)",
        params![
            chat,
            i64::try_from(crate::time::unix_seconds())?,
            workspace,
            about.id,
            kind,
            message,
            attachments
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
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
