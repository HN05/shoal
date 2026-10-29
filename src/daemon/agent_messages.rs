//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted. Files attached to a message outlive it, until their
//! workspace is removed.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
    state::WorkspaceState,
};

pub(crate) mod files;

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
    /// It waits for the agent's next turn or update.
    Quiet,
}

/// [`insert`] with `delivery`. An identical unread message that would not
/// wake the agent wakes it once queued again with [`Delivery::Wake`],
/// which returns its ID.
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
        if woke || !wakes {
            return Ok(None);
        }
        tx.execute("UPDATE agent_messages SET wakes=1 WHERE id=?1", [id])?;
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

/// Copies of `files`, attached to `message` for the workspace
/// `workspace_id`, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
async fn keep_message_files(
    layout: files::Layout,
    workspace_id: &str,
    message: &str,
    files: Vec<PathBuf>,
) -> Result<files::Kept> {
    let (id, message) = (workspace_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep(&id, &message, &files)).await?
}

/// Delete the copies of a message that was refused, such as one for a
/// workspace removed while they were made. Failure only logs: the workspace's
/// removal deletes whatever is left.
async fn discard_message_files(kept: files::Kept) {
    let discarded = tokio::task::spawn_blocking(move || kept.discard()).await;
    if let Err(error) = discarded
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        crate::daemon::log!("files attached to a refused message not deleted: {error:#}");
    }
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
    /// `attachments`; without text, the files are the message. An identical
    /// unread message is not repeated.
    pub async fn send_agent_message(
        &self,
        selector: &str,
        message: String,
        attachments: Vec<PathBuf>,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let workspace = self.workspace(selector).await?;
        if attachments.is_empty() {
            return self.queue_agent_message(&workspace.id, message).await;
        }
        let (layout, _files) = self.lock_message_files(&workspace).await?;
        let kept = keep_message_files(layout, &workspace.id, &message, attachments).await?;
        let queued = self
            .queue_attached_message(&workspace.id, message, kept.paths.clone(), Delivery::Wake)
            .await;
        if queued.is_err() {
            discard_message_files(kept).await;
        }
        queued
    }

    /// Hold the gate of the files attached to `workspace`'s messages, with
    /// where its repository keeps them, once the workspace is still there and
    /// not being removed. A registration older than the repository's
    /// directory reserves it first, under the Git gate, which is never taken
    /// while holding this one.
    async fn lock_message_files(
        &self,
        workspace: &Workspace,
    ) -> Result<(files::Layout, OwnedMutexGuard<()>)> {
        let repo = self.repository(&workspace.repository_id).await?;
        if repo.workspaces_dir.is_none() {
            let (repo, _git) = self.lock_repository(&repo.id).await?;
            self.workspaces_dir(&repo).await?;
        }
        let guard = self.gates.lock_message_files(&repo.id).await;
        // A move or removal may have finished while this waited.
        let current = self.workspace(&workspace.id).await?;
        ensure!(
            current.state != WorkspaceState::Removing,
            "the workspace is being removed"
        );
        let repo = self.repository(&repo.id).await?;
        let layout = files::Layout::of(&repo).context("the repository has no directory")?;
        Ok((layout, guard))
    }

    /// Delete the files attached to `workspace`'s messages once it is
    /// removed.
    pub(crate) async fn delete_message_files(&self, workspace: &Workspace) -> Result<()> {
        let _files = self
            .gates
            .lock_message_files(&workspace.repository_id)
            .await;
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(layout) = files::Layout::of(&repo) else {
            return Ok(());
        };
        let id = workspace.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_workspace(&id)).await?
    }

    /// Delete the directory of attached files with the repository, once its
    /// workspaces are gone.
    pub(crate) async fn delete_repository_message_files(&self, repo: &Repository) -> Result<()> {
        let _files = self.gates.lock_message_files(&repo.id).await;
        let Some(layout) = files::Layout::of(repo) else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || layout.remove()).await?
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
        self.queue_attached_message(workspace_id, message, Vec::new(), delivery)
            .await
    }

    async fn queue_attached_message(
        &self,
        workspace_id: &str,
        message: String,
        attachments: Vec<PathBuf>,
        delivery: Delivery,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let queued =
                    insert_delivered(&tx, &workspace_id, &message, &attachments, delivery)?;
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
        Ok(self.unread_agent_messages(selector).await?.messages)
    }

    /// Unread messages, oldest first, and whether one of them wakes an idle
    /// agent.
    pub(crate) async fn unread_agent_messages(&self, selector: &str) -> Result<Unread> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                let mut unread = Unread::default();
                let mut rows = db.prepare(
                    "SELECT id,created_at,message,attachments,wakes FROM agent_messages
                        WHERE workspace_id=?1 ORDER BY id",
                )?;
                let mut rows = rows.query([workspace.id])?;
                while let Some(row) = rows.next()? {
                    unread.messages.push(AgentMessage {
                        id: row.get("id")?,
                        created_at: row.get("created_at")?,
                        message: row.get("message")?,
                        attachments: crate::daemon::store::json_column(row, "attachments")?
                            .unwrap_or_default(),
                    });
                    unread.wakes |= row.get::<_, bool>("wakes")?;
                }
                Ok(unread)
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
