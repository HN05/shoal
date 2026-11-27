//! The store side of a conversion: changing a space's kind and swapping its
//! kind's row.
use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::daemon::Records;
use crate::model::EventCause;

use super::pending::{ConvertedChat, forget};
use crate::{
    config::{modes::ModeName, permissions::PermissionsConfig},
    model::SpaceKind,
    {
        approvals::{ApprovalRequest, drop_worktree_requests, release_decided_by},
        scope::tokens,
        spaces::records as spaces,
    },
};
use shoal_protocol::scope_tokens::ScopeToken;

/// A conversion keeps these as they are.
const CHAT_COLUMNS: &[&str] = &["id", "workspace_id", "root_dir"];

/// They name what the chat's agent ran in.
const SPACE_COLUMNS: &[&str] = &["repository_id", "name", "number", "mode", "narrowing"];

/// Its scope token, and its columns as a JSON object. Also the chat's
/// completion, which a failed conversion gives back with the row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRow {
    #[serde(default)]
    scope_token: Option<ScopeToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_completion: Option<KeptCompletion>,
    #[serde(flatten)]
    columns: serde_json::Map<String, serde_json::Value>,
}

/// The completion as JSON, which keeps its every field.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeptCompletion {
    record: String,
    cause: EventCause,
}

#[derive(Debug, Deserialize)]
pub struct ChatAgent {
    #[serde(default)]
    pub narrowing: Option<String>,
    #[serde(default)]
    pub scope_token: Option<ScopeToken>,
}

impl ChatRow {
    pub fn agent(&self) -> Result<ChatAgent> {
        let mut agent: ChatAgent =
            serde_json::from_value(serde_json::Value::Object(self.columns.clone()))?;
        agent.scope_token = self.scope_token.clone();
        Ok(agent)
    }

    fn mode(&self) -> Result<ModeName> {
        let mode = self.columns.get("mode").cloned();
        Ok(serde_json::from_value(
            mode.context("a chat's row names its mode")?,
        )?)
    }
}

impl ChatAgent {
    /// The permissions `--permission` gave it.
    pub fn narrowing(&self) -> Result<PermissionsConfig> {
        Ok((self.narrowing.as_deref())
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default())
    }
}

/// Does nothing when a stop saved the chat meanwhile and no other agent tool
/// `switches` to take its work over. The chat's threads, approval requests
/// and unread messages become the workspace's, and its agent's scope token,
/// which it keeps, acts for the workspace until the agent restarts there.
/// Returns the row the chat had and the execution its agent runs as, or a
/// new one for a stopped chat.
pub fn replace_chat(
    tx: &Transaction<'_>,
    id: &SpaceId,
    (mode, narrowing): (&ModeName, &PermissionsConfig),
    switches: bool,
) -> Result<(ChatRow, ExecutionId)> {
    // A chat's executions run its agent and nothing else.
    let chat = crate::store::exists(tx, "SELECT 1 FROM chats WHERE id=?1", [id])?;
    let execution = (crate::executions::newest_execution(tx, id)?)
        .filter(|_| chat)
        .or_else(|| (chat && switches).then(ExecutionId::generate))
        .context("the chat was stopped meanwhile; it stays a chat")?;
    let row = chat_row(tx, id)?;
    forget_completion(tx, id)?;
    spaces::convert(tx, id, SpaceKind::Workspace, mode, narrowing)?;
    tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
    Ok((row, execution))
}

/// The agent restarted under a scope of its own, so the chat's `token`
/// goes. So do the tokens the chat issued, as a workspace's exported
/// environments go with its worktree when it becomes a chat.
pub fn relocated_chat(
    tx: &Transaction<'_>,
    id: &SpaceId,
    token: Option<&ScopeToken>,
) -> Result<()> {
    if let Some(token) = token {
        tokens::revoke_agent(tx, id, token)?;
    }
    tokens::revoke_issued(tx, id)?;
    forget(tx, id)
}

/// The workspace's completion, threads, approval requests and unread
/// messages return to the chat. Of its scope tokens only the chat's own
/// stays.
pub fn restore_chat(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    row: &ChatRow,
) -> Result<()> {
    give_back_chat_row(tx, id, row)?;
    // The chat's agent, which has yet to move, runs on as the chat's.
    records.forget_command_executions(tx, id)?;
    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
    forget_completion(tx, id)?;
    if let Some(kept) = &row.space_completion {
        let completion = serde_json::from_str(&kept.record)?;
        crate::spaces::done::rows::write(tx, id, &completion, kept.cause)?;
    }
    forget(tx, id)
}

/// The chat restores in its repository's checkout. The workspace's threads,
/// unread messages and action requests become the chat's, and what only a
/// workspace has goes with its row. Returns the requests the workspace
/// decided, given back to the user, since a stopped chat decides none.
pub fn replace_workspace(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    chat: &ConvertedChat,
) -> Result<Vec<ApprovalRequest>> {
    tx.execute("INSERT INTO chats(id) VALUES (?1)", [id])?;
    spaces::convert(tx, id, SpaceKind::Chat, &chat.mode, &chat.narrowing)?;
    // The environments the workspace exported go with its worktree, and the
    // chat's agent carries a token of its own.
    tokens::clear(tx, id)?;
    tokens::issue_agent(tx, id)?;
    drop_worktree_requests(tx, id)?;
    let released = release_decided_by(tx, id)?;
    // Its executions, and the queue entries that restore or opened them, go
    // too: the record a conversion keeps does not take them with it.
    records.forget_executions(tx, id)?;
    forget_completion(tx, id)?;
    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
    forget(tx, id)?;
    Ok(released)
}

/// The completion ends with the kind the space had.
fn forget_completion(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_completion WHERE space_id=?1", [id])?;
    Ok(())
}

fn chat_row(db: &Connection, id: &SpaceId) -> Result<ChatRow> {
    Ok(ChatRow {
        scope_token: tokens::agent_token(db, id)?,
        space_completion: kept_completion(db, id)?,
        columns: chat_columns(db, id)?,
    })
}

/// The chat's columns, by name.
fn chat_columns(db: &Connection, id: &SpaceId) -> Result<Map<String, serde_json::Value>> {
    let names: Vec<&str> = (CHAT_COLUMNS.iter().chain(SPACE_COLUMNS))
        .copied()
        .collect();
    let values: Vec<SqlValue> = db
        .query_row(
            &format!(
                "SELECT {} FROM spaces JOIN chats USING(id) WHERE id=?1",
                names.join(",")
            ),
            [id],
            |row| (0..names.len()).map(|column| row.get(column)).collect(),
        )
        .optional()?
        .with_context(|| format!("no chat {id}"))?;
    (names.into_iter().zip(values))
        .map(|(name, value)| Ok((name.to_owned(), json_value(value)?)))
        .collect()
}

fn kept_completion(db: &Connection, id: &SpaceId) -> Result<Option<KeptCompletion>> {
    match crate::spaces::done::rows::read_with_cause(db, id)? {
        Some((completion, cause)) => Ok(Some(KeptCompletion {
            record: serde_json::to_string(&completion)?,
            cause,
        })),
        None => Ok(None),
    }
}

fn give_back_chat_row(tx: &Transaction<'_>, id: &SpaceId, row: &ChatRow) -> Result<()> {
    insert_chat(tx, row)?;
    let narrowing = row.agent()?.narrowing()?;
    spaces::convert(tx, id, SpaceKind::Chat, &row.mode()?, &narrowing)?;
    tokens::clear(tx, id)?;
    match &row.scope_token {
        Some(token) => tokens::record_agent(tx, id, token),
        None => Ok(()),
    }
}

/// Inserts the chat's row with the values `row` kept, null where it kept
/// none.
fn insert_chat(tx: &Transaction<'_>, row: &ChatRow) -> Result<()> {
    let values: Vec<SqlValue> = (CHAT_COLUMNS.iter())
        .map(|column| sql_value(row.columns.get(*column)))
        .collect::<Result<_>>()?;
    let placeholders: Vec<String> = (1..=values.len()).map(|n| format!("?{n}")).collect();
    tx.execute(
        &format!(
            "INSERT INTO chats({}) VALUES ({})",
            CHAT_COLUMNS.join(","),
            placeholders.join(",")
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

fn json_value(value: SqlValue) -> Result<serde_json::Value> {
    Ok(match value {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(number) => number.into(),
        SqlValue::Text(text) => text.into(),
        SqlValue::Real(_) | SqlValue::Blob(_) => bail!("a chat's column holds {value:?}"),
    })
}

fn sql_value(value: Option<&serde_json::Value>) -> Result<SqlValue> {
    Ok(match value {
        None | Some(serde_json::Value::Null) => SqlValue::Null,
        Some(serde_json::Value::String(text)) => SqlValue::Text(text.clone()),
        Some(serde_json::Value::Number(number)) => match number.as_i64() {
            Some(number) => SqlValue::Integer(number),
            None => bail!("a chat's row keeps {number} for a column"),
        },
        Some(other) => bail!("a chat's row keeps {other} for a column"),
    })
}

#[cfg(test)]
mod tests;
