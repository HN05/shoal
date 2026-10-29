//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted. Files attached to a message outlive it, until their
//! workspace is removed.
use std::path::PathBuf;

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{daemon::workspace::Manager, model::Workspace, state::WorkspaceState};

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
    let attachments = serde_json::to_string(attachments)?;
    let removing: Option<bool> = tx
        .query_row(
            "SELECT state=?2 FROM workspaces WHERE id=?1",
            params![workspace_id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(removing != Some(true), "the workspace is being removed");
    let pending: Option<i64> = tx
        .query_row(
            "SELECT id FROM agent_messages WHERE workspace_id=?1 AND message=?2 AND attachments=?3",
            params![workspace_id, message, attachments],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(None);
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
        "INSERT INTO agent_messages(workspace_id,created_at,message,attachments)
            VALUES (?1,?2,?3,?4)",
        params![
            workspace_id,
            i64::try_from(crate::time::unix_seconds())?,
            message,
            attachments
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
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
        let attachments = self
            .keep_message_files(&workspace, &message, attachments)
            .await?;
        self.queue_attached_message(&workspace.id, message, attachments)
            .await
    }

    /// Copies of `files`, attached to `message` for `workspace`'s agents.
    async fn keep_message_files(
        &self,
        workspace: &Workspace,
        message: &str,
        files: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>> {
        if files.is_empty() {
            return Ok(files);
        }
        let layout = self.message_files(workspace).await?;
        let (id, message) = (workspace.id.clone(), message.to_owned());
        tokio::task::spawn_blocking(move || layout.keep(&id, &message, &files)).await?
    }

    /// Where `workspace`'s repository keeps attached files, reserving the
    /// repository's directory for a registration older than it.
    async fn message_files(&self, workspace: &Workspace) -> Result<files::Layout> {
        let repo = self.repository(&workspace.repository_id).await?;
        if let Some(layout) = files::Layout::of(&repo) {
            return Ok(layout);
        }
        let (repo, _git) = self.lock_repository(&repo.id).await?;
        Ok(files::Layout::new(
            &self.workspaces_dir(&repo).await?,
            &repo.id,
        ))
    }

    /// Delete the files attached to `workspace`'s messages once it is
    /// removed.
    pub(crate) async fn delete_message_files(&self, workspace: &Workspace) -> Result<()> {
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(layout) = files::Layout::of(&repo) else {
            return Ok(());
        };
        let id = workspace.id.clone();
        tokio::task::spawn_blocking(move || layout.remove_workspace(&id)).await?
    }

    pub(crate) async fn queue_agent_message(
        &self,
        workspace_id: &str,
        message: String,
    ) -> Result<()> {
        self.queue_attached_message(workspace_id, message, Vec::new())
            .await
    }

    async fn queue_attached_message(
        &self,
        workspace_id: &str,
        message: String,
        attachments: Vec<PathBuf>,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let queued = insert(&tx, &workspace_id, &message, &attachments)?;
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
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,created_at,message,attachments FROM agent_messages
                            WHERE workspace_id=?1 ORDER BY id",
                    )?
                    .query_map([workspace.id], |row| {
                        Ok(AgentMessage {
                            id: row.get("id")?,
                            created_at: row.get("created_at")?,
                            message: row.get("message")?,
                            attachments: crate::daemon::store::json_column(row, "attachments")?
                                .unwrap_or_default(),
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
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
