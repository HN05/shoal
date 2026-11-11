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
    daemon::approvals::{ApprovalRequest, release_decided_by},
    process::identity::Identity,
    sessions::Origin,
    state::ChatRestore,
};

/// A conversion whose agent still runs where the space was, until its turn
/// ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub(crate) enum Pending {
    /// A chat that became a workspace, with the row the chat had, which
    /// gives its running agent and gives the chat back should the
    /// workspace's worktree fail, and the execution its agent's last
    /// session in the workspace is kept under.
    Workspace {
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
        /// The operation preparing the workspace, whose end, post-setup
        /// hook included, the agent waits for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preparation: Option<String>,
    },
    /// A workspace that becomes the chat `chat` once the agent it carries on
    /// stopped, with what its agent ran as and with, which outlives the
    /// agent's execution, as when the agent exits first.
    ///
    /// A stop of the workspace's agents that keeps them down until `shoal
    /// resume` `held` it: the workspace still becomes the chat, which stays
    /// stopped.
    Chat {
        chat: Box<ConvertedChat>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        held: bool,
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

/// A chat's row: its scope token, kept as one, and its other columns as a
/// JSON object, with what restarting its agent reads from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatRow {
    #[serde(default)]
    scope_token: Option<ScopeToken>,
    #[serde(flatten)]
    columns: serde_json::Map<String, serde_json::Value>,
}

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
        let mut agent: ChatAgent =
            serde_json::from_value(serde_json::Value::Object(self.columns.clone()))?;
        agent.scope_token = self.scope_token.clone();
        Ok(agent)
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

/// The processes the hand-overs of the conversion of the space `id` have
/// signalled, which a hand-over that was interrupted goes on stopping.
pub(crate) fn stopped_processes(db: &Connection, id: &SpaceId) -> Result<Vec<Identity>> {
    let stopped: Option<String> = db
        .query_row(
            "SELECT json_extract(record,'$.stopped') FROM space_conversions WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(stopped
        .map(|stopped| serde_json::from_str(&stopped))
        .transpose()?
        .unwrap_or_default())
}

/// Keep `signalled` among the processes the hand-overs of the conversion of
/// the space `id` have signalled, before a hand-over signals them.
pub(crate) fn record_stopped(
    tx: &Transaction<'_>,
    id: &SpaceId,
    signalled: &[Identity],
) -> Result<()> {
    let mut stopped = stopped_processes(tx, id)?;
    for process in signalled {
        if !stopped.contains(process) {
            stopped.push(process.clone());
        }
    }
    tx.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.stopped',json(?2))
         WHERE space_id=?1",
        params![id, serde_json::to_string(&stopped)?],
    )?;
    Ok(())
}

/// Keep `session` as the one the agent of the chat that became the
/// workspace `id` reported last, which its hand-over continues.
pub(crate) fn record_session(
    db: &Connection,
    id: &SpaceId,
    session: &AgentSessionId,
) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.chat.session',?2)
         WHERE space_id=?1 AND json_extract(record,'$.to')='workspace'",
        params![id, session],
    )?;
    Ok(())
}

/// Record `operation` as the one preparing the workspace `id` a chat
/// became, whose end its agent's restart there waits for.
pub(crate) fn record_preparation(db: &Connection, id: &SpaceId, operation: &str) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.preparation',?2)
         WHERE space_id=?1 AND json_extract(record,'$.to')='workspace'",
        params![id, operation],
    )?;
    Ok(())
}

/// Hold the conversion of the workspace `id` into a chat, whose agent the
/// process of the space `stopped_by`, or the user, stopped: the chat stays
/// stopped once the workspace becomes it.
pub(crate) fn hold_chat(db: &Connection, id: &SpaceId, stopped_by: Option<&SpaceId>) -> Result<()> {
    db.execute(
        "UPDATE space_conversions
         SET record=json_set(record,'$.held',json('true'),'$.chat.stopped_by',?2)
         WHERE space_id=?1 AND json_extract(record,'$.to')='chat'",
        params![id, stopped_by],
    )?;
    Ok(())
}

/// Forget the conversion of the space `id`, once its agent started in the
/// new place.
pub(crate) fn forget(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_conversions WHERE space_id=?1", [id])?;
    Ok(())
}

/// Record that the workspace `id` becomes `chat` once its agent's turn ends,
/// while the workspace is ready and that agent's execution runs, as a stop,
/// which first reserves the workspace, has not ended it.
pub(crate) fn record_chat_conversion(
    tx: &Transaction<'_>,
    id: &SpaceId,
    chat: ConvertedChat,
) -> Result<()> {
    anyhow::ensure!(
        crate::daemon::store::exists(
            tx,
            "SELECT 1 FROM workspaces JOIN executions ON executions.workspace_id=workspaces.id
             WHERE workspaces.id=?1 AND workspaces.state=?2 AND executions.id=?3
                AND executions.state=?4",
            params![
                id,
                crate::state::WorkspaceState::Ready,
                chat.execution,
                crate::state::ExecutionState::Running
            ],
        )?,
        "the workspace's agent was stopped meanwhile; it stays a workspace"
    );
    let pending = Pending::Chat {
        chat: Box::new(chat),
        held: false,
    };
    record(tx, id, &pending)
}

/// Replace the chat `id`, whose workspace row was just inserted under its
/// ID, with that workspace, unless a stop saved the chat meanwhile: its
/// questions and unread messages become the workspace's, and its agent's
/// scope token reaches the workspace until the agent restarts there.
/// Returns the row the chat had.
pub(crate) fn replace_chat(tx: &Transaction<'_>, id: &SpaceId) -> Result<ChatRow> {
    anyhow::ensure!(
        crate::daemon::store::exists(
            tx,
            "SELECT 1 FROM chats WHERE id=?1 AND restore IS NULL",
            [id]
        )?,
        "the chat was stopped meanwhile; it stays a chat"
    );
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

/// Forget the conversion of the chat that became the workspace `id`, whose
/// agent, which carried the scope `token`, restarted there under a scope of
/// its own.
pub(crate) fn relocated_chat(
    tx: &Transaction<'_>,
    id: &SpaceId,
    token: Option<&ScopeToken>,
) -> Result<()> {
    if let Some(token) = token {
        tx.execute("DELETE FROM workspace_scopes WHERE token=?1", [token])?;
    }
    forget(tx, id)
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

/// The stopped chat a workspace becomes: its agent, the execution it ran
/// as, the session it carries on and the place it ran in, the process it
/// ran as, its mode and the permissions given to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConvertedChat {
    pub agent: Agent,
    pub execution: ExecutionId,
    pub session: Option<AgentSessionId>,
    pub origin: Option<Origin>,
    pub process: Identity,
    pub mode: ModeName,
    pub narrowing: PermissionsConfig,
    /// The space whose process stopped the agent, which with the spaces
    /// above it may resume the chat; none for the user or Shoal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<SpaceId>,
}

/// Replace the workspace `id` with the stopped chat `chat` under its ID,
/// number and name, which restores in its repository's checkout: its
/// questions and unread messages become the chat's, and what only a
/// workspace has goes with its row. Returns the requests the workspace
/// decided, given back to the user, since a stopped chat decides none.
pub(crate) fn replace_workspace(
    tx: &Transaction<'_>,
    id: &SpaceId,
    chat: &ConvertedChat,
) -> Result<Vec<ApprovalRequest>> {
    tx.execute(
        "INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at,origin,session,
            restore,stopped_at,saved,unique_id,scope_token,number,mode,narrowing,converted,
            stopped_by)
         SELECT id,repository_id,name,?2,?3,?4,unixepoch(),?5,?6,?7,unixepoch(),1,?8,?9,
            number,?10,?11,1,?12
         FROM workspaces WHERE id=?1",
        params![
            id,
            chat.agent,
            chat.process.pid,
            chat.process.birth,
            (chat.origin.as_ref())
                .map(serde_json::to_string)
                .transpose()?,
            chat.session,
            ChatRestore::Resume,
            crate::daemon::chats::unique_id(&chat.process)?,
            ScopeToken::generate(),
            chat.mode,
            crate::daemon::store::narrowing_column(&chat.narrowing)?,
            chat.stopped_by,
        ],
    )?;
    tx.execute(
        "UPDATE questions SET chat_id=workspace_id,workspace_id=NULL WHERE workspace_id=?1",
        [id],
    )?;
    move_agent_messages(tx, id)?;
    let released = release_decided_by(tx, id)?;
    tx.execute("DELETE FROM executions WHERE workspace_id=?1", [id])?;
    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
    forget(tx, id)?;
    Ok(released)
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

#[cfg(test)]
mod tests;
