//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted. Files attached to a message outlive it, until their
//! workspace is removed (see [`crate::daemon::workspace_files`]).
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        workspace::Manager,
        workspace_files::{self, Attacher, NewFile, layout::Copy},
    },
    state::WorkspaceState,
};

mod settle;

/// Unread messages one workspace may hold, and their total size, attached
/// paths included; together they fit one response.
pub(crate) const MAX_UNREAD: i64 = 50;
pub(crate) const MAX_UNREAD_BYTES: usize = 50 * crate::validate::MAX_MESSAGE_LEN;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// Empty when the message is only its attachments.
    pub message: String,
    /// Shoal's copies of the files attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
}

/// Queue `message` with the files `attachments`, which Shoal has copied, in
/// the caller's transaction, returning its ID, or `None` when an identical
/// message is still unread. Callers wake watches with
/// `agent_messages_changed` after committing. A workspace being removed
/// takes none, since removal decides from the messages it has.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    workspace_id: &str,
    message: &str,
    attachments: &[PathBuf],
) -> Result<Option<i64>> {
    insert_delivered(tx, workspace_id, message, attachments, Delivery::Wake)
}

/// How a queued message reaches an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// It starts the agent's next turn.
    Wake,
    /// It starts the agent's next turn only while a tracked command, demo or
    /// background process runs in the workspace, as one the agent could stop;
    /// otherwise it waits for the agent's next turn or update.
    WhileWorkRuns,
}

/// [`insert`] with `delivery`. An identical unread message that would not
/// always wake the agent wakes it once queued again with [`Delivery::Wake`];
/// queued again with [`Delivery::WhileWorkRuns`], it returns its ID too, so
/// waits check again whether work runs.
pub(crate) fn insert_delivered(
    tx: &Transaction<'_>,
    workspace_id: &str,
    message: &str,
    attachments: &[PathBuf],
    delivery: Delivery,
) -> Result<Option<i64>> {
    let attachments = serde_json::to_string(attachments)?;
    let removing: Option<bool> = tx
        .query_row(
            "SELECT state=?2 FROM workspaces WHERE id=?1",
            params![workspace_id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(removing != Some(true), "the workspace is being removed");
    let wakes = delivery == Delivery::Wake;
    let pending: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id,wakes FROM agent_messages
                WHERE workspace_id=?1 AND message=?2 AND attachments=?3",
            params![workspace_id, message, attachments],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, woke)) = pending {
        if woke {
            return Ok(None);
        }
        if wakes {
            tx.execute("UPDATE agent_messages SET wakes=1 WHERE id=?1", [id])?;
        }
        return Ok(Some(id));
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(LENGTH(CAST(message||attachments AS BLOB))),0)
            FROM agent_messages WHERE workspace_id=?1",
        [workspace_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the workspace already has {MAX_UNREAD} unread agent messages"
    );
    ensure!(
        usize::try_from(bytes)? + message.len() + attachments.len() <= MAX_UNREAD_BYTES,
        "the workspace's unread agent messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO agent_messages(workspace_id,created_at,message,attachments,wakes)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            workspace_id,
            i64::try_from(crate::time::unix_seconds())?,
            message,
            attachments,
            wakes
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

/// Point the attached paths of messages in the workspaces of repository
/// `repository_id` from `source` to `destination`, in the transaction that
/// moves the repository's directory.
pub(crate) fn rebase_attachments(
    tx: &Transaction<'_>,
    repository_id: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let attached: Vec<(i64, String)> = tx
        .prepare(
            "SELECT m.id,m.attachments FROM agent_messages m
                JOIN workspaces w ON w.id=m.workspace_id
                WHERE w.repository_id=?1 AND m.attachments!='[]'",
        )?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, attachments) in attached {
        let paths: Vec<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(&attachments)?
            .into_iter()
            .map(|path| match path.strip_prefix(source) {
                Ok(inside) => destination.join(inside),
                Err(_) => path,
            })
            .collect();
        tx.execute(
            "UPDATE agent_messages SET attachments=?2 WHERE id=?1",
            params![id, serde_json::to_string(&paths)?],
        )?;
    }
    Ok(())
}

/// A workspace's unread messages.
#[derive(Debug, Default)]
pub(crate) struct Unread {
    pub messages: Vec<AgentMessage>,
    /// Whether one of them starts an idle agent's next turn.
    pub wakes: bool,
}

impl Manager {
    /// Queue a message for the workspace's agents with copies of the files
    /// `attachments`, attached by `attacher`; without text, the files are the
    /// message. An identical unread message is not repeated.
    pub async fn send_agent_message(
        &self,
        selector: &str,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let workspace = self.workspace(selector).await?;
        if attachments.is_empty() {
            return self.queue_agent_message(&workspace.id, message).await;
        }
        let (layout, _files) = self.lock_files(&workspace).await?;
        let kept = workspace_files::keep(layout, &workspace.id, &message, attachments).await?;
        let queued = self
            .queue_attached_message(
                &workspace.id,
                message,
                kept.copies.clone(),
                Some(attacher.clone()),
                Delivery::Wake,
            )
            .await;
        if queued.is_err() {
            workspace_files::discard(kept).await;
        }
        queued
    }

    pub(crate) async fn queue_agent_message(
        &self,
        workspace_id: &str,
        message: String,
    ) -> Result<()> {
        self.queue_agent_message_delivered(workspace_id, message, Delivery::Wake)
            .await
    }

    pub(crate) async fn queue_agent_message_delivered(
        &self,
        workspace_id: &str,
        message: String,
        delivery: Delivery,
    ) -> Result<()> {
        self.queue_attached_message(workspace_id, message, Vec::new(), None, delivery)
            .await
    }

    /// Queue `message` with Shoal's copies `attachments`, which the
    /// workspace's attached files list from then on as attached by
    /// `attacher`.
    async fn queue_attached_message(
        &self,
        workspace_id: &str,
        message: String,
        attachments: Vec<Copy>,
        attacher: Option<Attacher>,
        delivery: Delivery,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let paths: Vec<_> = attachments.iter().map(|copy| copy.path.clone()).collect();
                let queued = insert_delivered(&tx, &workspace_id, &message, &paths, delivery)?;
                workspace_files::record(
                    &tx,
                    &workspace_id,
                    workspace_files::FileSource::Message,
                    attacher.as_ref(),
                    &attachments,
                )?;
                tx.commit()?;
                Ok(queued.is_some())
            })
            .await?;
        if queued {
            self.signals.agent_message_queued();
        }
        Ok(())
    }

    /// Unread messages, oldest first. They stay unread until delivered.
    pub async fn agent_messages(&self, selector: &str) -> Result<Vec<AgentMessage>> {
        let workspace = self.workspace(selector).await?;
        Ok(self.read_unread(workspace.id).await?.0.messages)
    }

    /// Unread messages, oldest first, and whether one of them wakes an idle
    /// agent now. One queued [`Delivery::WhileWorkRuns`] wakes it only while
    /// work it could stop runs, or when that cannot be read.
    pub(crate) async fn unread_agent_messages(&self, selector: &str) -> Result<Unread> {
        let workspace = self.workspace(selector).await?;
        let (mut unread, while_work_runs) = self.read_unread(workspace.id.clone()).await?;
        if !unread.wakes && while_work_runs {
            unread.wakes = self
                .runs_agent_work(&workspace.id)
                .await
                .unwrap_or_else(|error| {
                    crate::daemon::log!("running work unreadable, so messages wake: {error:#}");
                    true
                });
        }
        Ok(unread)
    }

    /// The workspace's unread messages, with whether one always wakes an
    /// idle agent and whether one wakes it while work runs.
    async fn read_unread(&self, workspace_id: String) -> Result<(Unread, bool)> {
        self.store
            .run(move |db| {
                let mut unread = Unread::default();
                let mut while_work_runs = false;
                let mut rows = db.prepare(
                    "SELECT id,created_at,message,attachments,wakes FROM agent_messages
                        WHERE workspace_id=?1 ORDER BY id",
                )?;
                let mut rows = rows.query([workspace_id])?;
                while let Some(row) = rows.next()? {
                    unread.messages.push(AgentMessage {
                        id: row.get("id")?,
                        created_at: row.get("created_at")?,
                        message: row.get("message")?,
                        attachments: crate::daemon::store::json_column(row, "attachments")?
                            .unwrap_or_default(),
                    });
                    // A message that does not always wake waits for work.
                    if row.get::<_, bool>("wakes")? {
                        unread.wakes = true;
                    } else {
                        while_work_runs = true;
                    }
                }
                Ok((unread, while_work_runs))
            })
            .await
    }

    /// Acknowledge the item updates and messages an agent received.
    pub async fn acknowledge_agent_delivery(
        &self,
        selector: &str,
        deliveries: Vec<String>,
        messages: Vec<i64>,
    ) -> Result<()> {
        // Item updates first: a failure then leaves both to be delivered
        // again, while deleted messages could not be.
        self.acknowledge_pr_updates(selector, deliveries).await?;
        self.mark_agent_messages_delivered(selector, messages).await
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
        self.signals.agent_changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
