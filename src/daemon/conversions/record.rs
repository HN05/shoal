//! The store side of a conversion: moving a space's row between `chats` and
//! `workspaces` under the same ID and number, with what belongs to only one
//! kind's row, and the record of a conversion whose agent has yet to start
//! in the new place.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{
    agent::Agent,
    client::{agent::AgentSessionId, scope_tokens::ScopeToken},
    config::{modes::ModeName, permissions::PermissionsConfig},
    process::identity::Identity,
    sessions::Origin,
};

/// A conversion whose agent still runs where the space was, until its turn
/// ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub(crate) enum Pending {
    /// A chat that became a workspace, with the row the chat had, which
    /// gives its running agent and gives the chat back should the
    /// workspace's worktree fail.
    Workspace { mode: ModeName, chat: ChatRow },
    /// A workspace that becomes a chat once the agent `execution` runs
    /// stopped.
    Chat {
        mode: ModeName,
        execution: ExecutionId,
    },
}

/// The columns of a chat's row, which a conversion keeps as they are.
const CHAT_COLUMNS: &[&str] = &[
    "id",
    "repository_id",
    "name",
    "agent",
    "pid",
    "birth",
    "started_at",
    "origin",
    "starting",
    "session",
    "restore",
    "unique_id",
    "asks_in_chat",
    "completion",
    "stopped_at",
    "saved",
    "scope_token",
    "workspace_id",
    "number",
    "mode",
    "root_dir",
    "stopped_by",
    "narrowing",
    "converted",
];

/// A chat's row, as a JSON object of its columns, with what restarting its
/// agent reads from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ChatRow(serde_json::Map<String, serde_json::Value>);

/// What a converted chat's agent ran as and with.
#[derive(Debug, Deserialize)]
pub(crate) struct ChatAgent {
    pub agent: Agent,
    pub pid: u32,
    pub birth: String,
    #[serde(default)]
    pub unique_id: Option<String>,
    #[serde(default)]
    pub session: Option<AgentSessionId>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub narrowing: Option<String>,
    #[serde(default)]
    pub scope_token: Option<ScopeToken>,
}

impl ChatRow {
    /// The agent the row records.
    pub(crate) fn agent(&self) -> Result<ChatAgent> {
        Ok(serde_json::from_value(serde_json::Value::Object(
            self.0.clone(),
        ))?)
    }
}

impl ChatAgent {
    /// The process the agent ran as.
    pub(crate) fn process(&self) -> Result<Identity> {
        Ok(Identity {
            pid: self.pid,
            birth: self.birth.clone(),
            unique: (self.unique_id.as_deref())
                .map(serde_json::from_str)
                .transpose()?,
        })
    }

    /// Where it ran in a terminal host.
    pub(crate) fn origin(&self) -> Result<Option<Origin>> {
        Ok((self.origin.as_deref())
            .map(serde_json::from_str)
            .transpose()?)
    }

    /// The permissions `--permission` gave it.
    pub(crate) fn narrowing(&self) -> Result<PermissionsConfig> {
        Ok((self.narrowing.as_deref())
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default())
    }
}

/// Record `pending` for the space `id`.
pub(crate) fn record(tx: &Transaction<'_>, id: &SpaceId, pending: &Pending) -> Result<()> {
    tx.execute(
        "INSERT INTO space_conversions(space_id,record) VALUES (?1,?2)",
        params![id, serde_json::to_string(pending)?],
    )?;
    Ok(())
}

/// The conversion of the space `id` whose agent has yet to start in the
/// new place.
pub(crate) fn pending(db: &Connection, id: &SpaceId) -> Result<Option<Pending>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_conversions WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

/// Every conversion whose agent has yet to start in the new place.
pub(crate) fn all_pending(db: &Connection) -> Result<Vec<(SpaceId, Pending)>> {
    db.prepare("SELECT space_id,record FROM space_conversions")?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .map(|row| {
            let (id, record) = row?;
            Ok((id, serde_json::from_str(&record)?))
        })
        .collect()
}

/// Forget the conversion of the space `id`, once its agent started in the
/// new place.
pub(crate) fn forget(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_conversions WHERE space_id=?1", [id])?;
    Ok(())
}

/// Replace the chat `id`, whose workspace row was just inserted under its
/// ID, with that workspace: its questions and unread messages become the
/// workspace's, and its agent's scope token reaches the workspace until the
/// agent restarts there. Returns the row the chat had.
pub(crate) fn replace_chat(tx: &Transaction<'_>, id: &SpaceId) -> Result<ChatRow> {
    let row = chat_row(tx, id)?;
    tx.execute(
        "UPDATE questions SET workspace_id=chat_id,chat_id=NULL WHERE chat_id=?1",
        [id],
    )?;
    tx.execute(
        "INSERT INTO agent_messages(workspace_id,created_at,message,attachments)
            SELECT chat_id,created_at,
                CASE workspace WHEN '' THEN message ELSE workspace || ': ' || message END,
                attachments
            FROM chat_messages WHERE chat_id=?1 ORDER BY id",
        [id],
    )?;
    tx.execute(
        "INSERT INTO workspace_scopes(token,workspace_id)
            SELECT scope_token,id FROM chats WHERE id=?1 AND scope_token IS NOT NULL",
        [id],
    )?;
    tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
    Ok(row)
}

/// Give back the chat `row`, which the workspace `id` replaced, and remove
/// that workspace's row: its questions and unread messages return to the
/// chat.
pub(crate) fn restore_chat(tx: &Transaction<'_>, id: &SpaceId, row: &ChatRow) -> Result<()> {
    insert_chat_row(tx, row)?;
    tx.execute(
        "UPDATE questions SET chat_id=workspace_id,workspace_id=NULL WHERE workspace_id=?1",
        [id],
    )?;
    move_agent_messages(tx, id)?;
    tx.execute("DELETE FROM executions WHERE workspace_id=?1", [id])?;
    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
    forget(tx, id)
}

/// Make the unread agent messages of the workspace `id` the unread messages
/// of the chat that replaces it under that ID.
fn move_agent_messages(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "INSERT INTO chat_messages(chat_id,created_at,workspace,message,attachments)
            SELECT workspace_id,created_at,'',message,attachments
            FROM agent_messages WHERE workspace_id=?1 ORDER BY id",
        [id],
    )?;
    Ok(())
}

/// The row of the chat `id`.
fn chat_row(db: &Connection, id: &SpaceId) -> Result<ChatRow> {
    let pairs: Vec<String> = (CHAT_COLUMNS.iter())
        .map(|column| format!("'{column}',{column}"))
        .collect();
    let row: String = db
        .query_row(
            &format!(
                "SELECT json_object({}) FROM chats WHERE id=?1",
                pairs.join(",")
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?
        .with_context(|| format!("no chat {id}"))?;
    Ok(serde_json::from_str(&row)?)
}

/// Insert the chat `row` as it was.
fn insert_chat_row(tx: &Transaction<'_>, row: &ChatRow) -> Result<()> {
    let values: Vec<String> = (CHAT_COLUMNS.iter())
        .map(|column| format!("json_extract(?1,'$.{column}')"))
        .collect();
    tx.execute(
        &format!(
            "INSERT INTO chats({}) VALUES ({})",
            CHAT_COLUMNS.join(","),
            values.join(",")
        ),
        [serde_json::to_string(row)?],
    )?;
    Ok(())
}
