//! The store side of a conversion: changing a space's kind and swapping its
//! kind's row, and the record of a conversion whose agent has yet to start
//! in the new place.
use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::daemon::Records;

use super::handoff::Taking;
use crate::{
    agent::Agent,
    config::{modes::ModeName, permissions::PermissionsConfig},
    model::SpaceKind,
    sessions::Origin,
    {
        approvals::{ApprovalRequest, drop_worktree_requests, release_decided_by},
        scope::tokens,
        spaces::records as spaces,
    },
};
use shoal_protocol::scope_tokens::ScopeToken;

/// The agent still runs where the space was, until its turn ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Pending {
    /// A chat that became a workspace. The row the chat had gives its running
    /// agent, and gives the chat back should the workspace's worktree fail.
    /// The agent runs as `execution`, whose session files hold its session
    /// and under which its last session in the workspace is kept.
    Workspace {
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
        /// The agent waits for this preparation to end, post-setup hook
        /// included.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preparation: Option<String>,
        /// Kept beyond the operation's own record.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        prepared: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_failure: Option<String>,
        #[serde(default, skip_serializing_if = "Carrying::is_default")]
        carrying: Carrying,
    },
    /// The agent's details outlive its execution, as when the agent exits
    /// first. A stop that keeps the workspace's agents down until `shoal
    /// resume` `held` it: the workspace still becomes the chat, which stays
    /// stopped.
    Chat {
        chat: Box<ConvertedChat>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        held: bool,
        #[serde(default, skip_serializing_if = "Carrying::is_default")]
        carrying: Carrying,
    },
    /// Runs once the turn of the agent `execution` runs ends. A stop that
    /// keeps that agent down until `shoal resume` `held` it: `taking` then
    /// waits stopped too.
    Agent {
        execution: ExecutionId,
        taking: Box<Taking>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        held: bool,
    },
}

impl Pending {
    /// Rather than leave it for `shoal resume`: true unless a stop held it or
    /// the new workspace's post-setup hook failed.
    pub fn restarts_agent(&self) -> bool {
        match self {
            Self::Workspace { hook_failure, .. } => hook_failure.is_none(),
            Self::Chat { held, .. } | Self::Agent { held, .. } => !held,
        }
    }
}

/// Besides its mode: with the model asked of it, from a fork of its session,
/// or else as the agent tool `taking` names takes its work over.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Carrying {
    #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
    pub model: crate::agent::ModelChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taking: Option<Box<Taking>>,
}

impl Carrying {
    fn is_default(&self) -> bool {
        self.model.is_empty() && self.taking.is_none()
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeptCompletion {
    record: String,
    cause: String,
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

pub fn record(tx: &Transaction<'_>, id: &SpaceId, pending: &Pending) -> Result<()> {
    tx.execute(
        "INSERT INTO space_conversions(space_id,record) VALUES (?1,?2)",
        params![id, serde_json::to_string(pending)?],
    )?;
    Ok(())
}

/// One whose agent has yet to start in the new place.
pub fn pending(db: &Connection, id: &SpaceId) -> Result<Option<Pending>> {
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

/// Those whose agent has yet to start in the new place.
pub fn all_pending(db: &Connection) -> Result<Vec<(SpaceId, Pending)>> {
    db.prepare("SELECT space_id,record FROM space_conversions")?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .map(|row| {
            let (id, record) = row?;
            Ok((id, serde_json::from_str(&record)?))
        })
        .collect()
}

/// The agent's restart in the workspace waits for its end.
pub fn record_preparation(db: &Connection, id: &SpaceId, operation: &str) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.preparation',?2)
         WHERE space_id=?1 AND json_extract(record,'$.to')='workspace'",
        params![id, operation],
    )?;
    Ok(())
}

/// Records why its post-setup hook failed, when it did. An end reported
/// before the conversion recorded its preparation, which starts first,
/// records that preparation too.
pub fn record_prepared(
    db: &Connection,
    id: &SpaceId,
    operation: &str,
    hook_failure: Option<&str>,
) -> Result<()> {
    db.execute(
        "UPDATE space_conversions
         SET record=json_set(record,'$.preparation',?2,'$.prepared',json('true'),
            '$.hook_failure',?3)
         WHERE space_id=?1 AND json_extract(record,'$.to')='workspace'
            AND COALESCE(json_extract(record,'$.preparation'),?2)=?2",
        params![id, operation, hook_failure],
    )?;
    Ok(())
}

/// The agent that carries on stays stopped once the conversion is made.
pub fn hold(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.held',json('true'))
         WHERE space_id=?1 AND json_extract(record,'$.to') IN ('chat','agent')",
        [id],
    )?;
    Ok(())
}

/// Runs once its agent started in the new place.
pub fn forget(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_conversions WHERE space_id=?1", [id])?;
    Ok(())
}

/// Records only while the workspace is ready and the agent's execution
/// runs, as a stop, which first reserves the workspace, has not ended it.
/// An agent tool that takes the work over takes it from a stopped agent
/// too.
pub fn record_chat_conversion(
    tx: &Transaction<'_>,
    id: &SpaceId,
    chat: ConvertedChat,
    carrying: Carrying,
) -> Result<()> {
    let ready = crate::workspace::is_in_state(tx, id, crate::state::WorkspaceState::Ready)?;
    let running = crate::executions::execution_state(tx, &chat.execution)?
        == Some((id.to_owned(), crate::state::ExecutionState::Running));
    anyhow::ensure!(
        ready && (running || carrying.taking.is_some()),
        "the workspace's agent was stopped meanwhile; it stays a workspace"
    );
    let pending = Pending::Chat {
        chat: Box::new(chat),
        held: false,
        carrying,
    };
    record(tx, id, &pending)
}

/// Records only while that agent's execution runs.
pub fn record_agent_switch(
    tx: &Transaction<'_>,
    id: &SpaceId,
    execution: ExecutionId,
    taking: Taking,
) -> Result<()> {
    let running = crate::executions::execution_state(tx, &execution)?
        == Some((id.to_owned(), crate::state::ExecutionState::Running));
    ensure!(running, "the agent was stopped meanwhile; convert it again");
    let pending = Pending::Agent {
        execution,
        taking: Box::new(taking),
        held: false,
    };
    record(tx, id, &pending)
}

/// No agent starts there until the space's sessions are copied.
pub fn record_stopped_switch(
    tx: &Transaction<'_>,
    id: &SpaceId,
    execution: ExecutionId,
    taking: Taking,
) -> Result<()> {
    let pending = Pending::Agent {
        execution,
        taking: Box::new(taking),
        held: false,
    };
    record(tx, id, &pending)
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
    if let Some(completion) = &row.space_completion {
        tx.execute(
            "INSERT OR REPLACE INTO space_completion(space_id,record,cause) VALUES (?1,?2,?3)",
            params![id, completion.record, completion.cause],
        )?;
    }
    forget(tx, id)
}

/// Its agent, the execution it ran as, whose session it carries on, the
/// place it ran in, its mode and the permissions given to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertedChat {
    pub agent: Agent,
    pub execution: ExecutionId,
    pub origin: Option<Origin>,
    pub mode: ModeName,
    pub narrowing: PermissionsConfig,
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
    let pairs: Vec<String> = (CHAT_COLUMNS.iter().chain(SPACE_COLUMNS))
        .map(|column| format!("'{column}',{column}"))
        .collect();
    let row: String = db
        .query_row(
            &format!(
                "SELECT json_object({}) FROM spaces JOIN chats USING(id) WHERE id=?1",
                pairs.join(",")
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?
        .with_context(|| format!("no chat {id}"))?;
    let mut row: ChatRow = serde_json::from_str(&row)?;
    row.scope_token = tokens::agent_token(db, id)?;
    row.space_completion = db
        .query_row(
            "SELECT record,cause FROM space_completion WHERE space_id=?1",
            [id],
            |row| {
                Ok(KeptCompletion {
                    record: row.get(0)?,
                    cause: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn give_back_chat_row(tx: &Transaction<'_>, id: &SpaceId, row: &ChatRow) -> Result<()> {
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
    let narrowing = row.agent()?.narrowing()?;
    spaces::convert(tx, id, SpaceKind::Chat, &row.mode()?, &narrowing)?;
    tokens::clear(tx, id)?;
    match &row.scope_token {
        Some(token) => tokens::record_agent(tx, id, token),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
