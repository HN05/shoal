//! Messages for a chat about the workspaces it coordinates, and from the
//! user. Like agent messages, delivery deletes them; a chat removed before
//! receiving them gives them to the user as the notifications they would
//! have been.
use crate::client::space_ids::SpaceId;
use crate::daemon::links::wait::UpdatesExt;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

use std::path::{Path, PathBuf};

use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::daemon::{
    agent_messages::{self, AgentMessage, MAX_UNREAD, MAX_UNREAD_BYTES},
    links::wait::{RESPONSE_LIMIT, Updates},
    notifications::{self, About, NotificationKind},
    workspace::Manager,
    workspace_files::{self, Attacher, NewFile, layout::Copy},
};

/// How soon a chat's wait looks again at item activity an item change held
/// back under its PR gate.
const GATE_RETRY: std::time::Duration = std::time::Duration::from_millis(500);

impl Manager {
    /// Queue a message from the user for the chat `chat` names by ID
    /// or name, with copies of `attachments`, which the chat's attached files
    /// list from then on as attached by `attacher`; without text, the files
    /// are the message. It is dropped if the chat is removed first.
    pub async fn send_chat_message(
        &self,
        chat: &SpaceSelector,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let chat = self.chat(chat).await?;
        if attachments.is_empty() {
            return self
                .queue_chat_message(&chat.id, None, None, None, message, Vec::new())
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
        chat: SpaceId,
        message: String,
        attachments: Vec<Copy>,
        attacher: Attacher,
    ) -> Result<()> {
        let queued = self
            .store
            .write(move |tx| {
                let paths: Vec<_> = attachments.iter().map(|copy| copy.path.clone()).collect();
                let queued = insert(tx, &chat, &About::default(), None, &message, &paths)?;
                let queued = queued.is_some();
                let source = workspace_files::FileSource::Message;
                workspace_files::record(tx, &chat, source, Some(&attacher), &attachments)?;
                Ok(queued)
            })
            .await?;
        if queued {
            self.signals.chat_message_queued();
        }
        Ok(())
    }

    /// Queue `message` for the chat, about the workspace named `workspace`
    /// it coordinates, if any, with Shoal's copies `attachments` of the files
    /// that workspace sent. `kind` is the notification the user receives
    /// instead if the chat is removed first; without one the message is dropped
    /// then. An identical unread message is not repeated.
    pub(crate) async fn queue_chat_message(
        &self,
        chat: &SpaceId,
        workspace: Option<&SpaceName>,
        workspace_id: Option<&SpaceId>,
        kind: Option<NotificationKind>,
        message: String,
        attachments: Vec<PathBuf>,
    ) -> Result<()> {
        let (chat, workspace) = (chat.to_owned(), workspace.cloned());
        let workspace_id = workspace_id.cloned();
        let queued = self
            .store
            .write(move |tx| {
                let about = About {
                    name: workspace,
                    id: workspace_id,
                    subject: None,
                };
                let queued = insert(tx, &chat, &about, kind, &message, &attachments)?.is_some();
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
    pub async fn chat_messages(&self, chat: &SpaceId) -> Result<Vec<AgentMessage>> {
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

    /// What the chat's agent has not received, without waiting: its
    /// messages, its leftovers, and the activity queued on its items.
    /// Delivery replaces a running wait, which would otherwise deliver them
    /// again.
    pub async fn chat_updates(&self, chat: &SpaceId) -> Result<Updates> {
        let delivery = self.chat_waits.start(chat);
        let messages = self.chat_messages(chat).await?;
        // The messages, then the leftovers, share the response with the
        // updates.
        let messages_size = serde_json::to_vec(&messages)?.len();
        let room = RESPONSE_LIMIT.saturating_sub(messages_size);
        let leftovers = self.leftovers.undelivered(chat, room)?;
        let reserved = messages_size + serde_json::to_vec(&leftovers)?.len();
        let updates = (self.queued_chat_updates(chat, reserved).await?).unwrap_or_default();
        if !delivery.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates {
            updates,
            leftovers,
            ..Updates::messages(messages)
        })
    }

    /// The chat's next messages or item activity, once it has any, unless a
    /// newer wait or delivery takes them first.
    pub async fn wait_chat_messages(&self, chat: &SpaceId) -> Result<Updates> {
        let wait = self.chat_waits.start(chat);
        // Subscribe before reading, so what is queued in between still wakes.
        let mut queued = self.signals.watch_chat_messages();
        let mut background = self.signals.watch_item_updates();
        loop {
            let messages = self.chat_messages(chat).await?;
            let reserved = serde_json::to_vec(&messages)?.len();
            let queued_updates = self.queued_chat_updates(chat, reserved).await?;
            // Releasing the PR gate signals nothing, so a held one is
            // checked again shortly.
            let gated = queued_updates.is_none();
            let updates = queued_updates.unwrap_or_default();
            if !messages.is_empty() || !updates.is_empty() {
                if !wait.is_current() {
                    return Ok(Updates::superseded());
                }
                return Ok(Updates {
                    updates,
                    ..Updates::messages(messages)
                });
            }
            tokio::select! {
                changed = queued.changed() => changed?,
                changed = background.changed() => changed?,
                () = tokio::time::sleep(GATE_RETRY), if gated => {}
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
    }

    /// Delete exactly these messages once the chat's agent has received
    /// them, with the questions whose answers they carried.
    pub async fn mark_chat_messages_delivered(&self, chat: &SpaceId, ids: Vec<i64>) -> Result<()> {
        let chat = chat.to_owned();
        self.store
            .write(move |tx| {
                for id in ids {
                    tx.execute(
                        "DELETE FROM chat_messages WHERE id=?1 AND chat_id=?2",
                        params![id, chat],
                    )?;
                    crate::daemon::questions::delete_delivered(tx, &chat, id)?;
                }
                Ok(())
            })
            .await
    }
}

/// Queue `message` from the user for the chat in the caller's transaction:
/// its ID, or `None` when an identical message is already unread.
pub(crate) fn insert_from_user(
    tx: &Transaction<'_>,
    chat: &SpaceId,
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
    chat: &SpaceId,
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
            crate::time::UnixTime::now(),
            workspace,
            about.id,
            kind,
            message,
            attachments
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

/// Point the attached paths of chats' messages from `source` to
/// `destination`, in the transaction that moves a repository's directory
/// there. A moving repository has no chat of its own, but a chat of another
/// may hold the files a workspace of it sent.
pub(crate) fn rebase_attachments(
    tx: &Transaction<'_>,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let attached: Vec<(i64, String)> = tx
        .prepare("SELECT id,attachments FROM chat_messages WHERE attachments!='[]'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    agent_messages::rewrite_attachments(tx, "chat_messages", attached, source, destination)
}

/// Give the user the notifications that the removed chat's unread messages
/// stand for, with the files they attach, in the transaction that deletes
/// the chat and with it the messages. Returns the last notification
/// recorded.
pub(super) fn hand_to_user(tx: &Transaction<'_>, chat: &SpaceId) -> Result<Option<i64>> {
    let unread = tx
        .prepare(
            "SELECT workspace,workspace_id,kind,message,attachments FROM chat_messages
                WHERE chat_id=?1 AND kind IS NOT NULL ORDER BY id",
        )?
        .query_map([chat], |row| {
            let about = About {
                name: Some(row.get(0)?),
                id: row.get(1)?,
                subject: None,
            };
            let attachments: Vec<PathBuf> =
                crate::daemon::store::json_column(row, "attachments")?.unwrap_or_default();
            Ok((about, row.get(2)?, row.get(3)?, attachments))
        })?
        .collect::<rusqlite::Result<Vec<(About, NotificationKind, String, Vec<PathBuf>)>>>()?;
    let mut last = None;
    for (about, kind, message, attachments) in unread {
        let files = workspace_files::ids_at(tx, &attachments)?;
        let recorded = notifications::insert(tx, &about, kind, &message, &files)?;
        if recorded.new {
            last = Some(recorded.id);
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests;
