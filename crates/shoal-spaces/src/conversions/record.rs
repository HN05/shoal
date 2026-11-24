//! The store side of a conversion: changing a space's kind and swapping its
//! kind's row, with what belongs to only one kind, and the record of a
//! conversion whose agent has yet to start in the new place.
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

/// A conversion whose agent still runs where the space was, until its turn
/// ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Pending {
    /// A chat that became a workspace, with the row the chat had, which
    /// gives its running agent and gives the chat back should the
    /// workspace's worktree fail, and the execution that agent runs as,
    /// whose session files hold its session and under which its last
    /// session in the workspace is kept.
    Workspace {
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
        /// The operation preparing the workspace, whose end, post-setup
        /// hook included, the agent waits for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preparation: Option<String>,
        /// Whether that preparation reported its end, kept beyond the
        /// operation's own record.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        prepared: bool,
        /// Why its post-setup hook failed, when it did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_failure: Option<String>,
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
    /// A space whose work `taking` takes over from the agent `execution`
    /// runs once its turn ends. A stop that keeps that agent down until
    /// `shoal resume` `held` it: `taking` then waits stopped too.
    Agent {
        execution: ExecutionId,
        taking: Box<Taking>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        held: bool,
    },
}

impl Pending {
    /// Whether the conversion restarts the agent in the new place, or starts
    /// the agent tool that takes the work over, rather than leave it for
    /// `shoal resume`: neither a stop held it nor the new workspace's
    /// post-setup hook failed.
    pub fn restarts_agent(&self) -> bool {
        match self {
            Self::Workspace { hook_failure, .. } => hook_failure.is_none(),
            Self::Chat { held, .. } | Self::Agent { held, .. } => !held,
        }
    }
}

/// The columns of a chat's row, which a conversion keeps as they are.
const CHAT_COLUMNS: &[&str] = &["id", "workspace_id", "root_dir"];

/// The columns of the chat's space's row it keeps with them, which name
/// what the chat's agent ran in.
const SPACE_COLUMNS: &[&str] = &["repository_id", "name", "number", "mode", "narrowing"];

/// A chat's row: its scope token, and its columns as a JSON object, with
/// what restarting its agent reads from it, and the chat's completion, which
/// a failed conversion gives back with the row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRow {
    #[serde(default)]
    scope_token: Option<ScopeToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_completion: Option<KeptCompletion>,
    #[serde(flatten)]
    columns: serde_json::Map<String, serde_json::Value>,
}

/// A completion as `space_completion` records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeptCompletion {
    record: String,
    cause: String,
}

/// What a converted chat's agent ran with.
#[derive(Debug, Deserialize)]
pub struct ChatAgent {
    #[serde(default)]
    pub narrowing: Option<String>,
    #[serde(default)]
    pub scope_token: Option<ScopeToken>,
}

impl ChatRow {
    /// The agent the row records.
    pub fn agent(&self) -> Result<ChatAgent> {
        let mut agent: ChatAgent =
            serde_json::from_value(serde_json::Value::Object(self.columns.clone()))?;
        agent.scope_token = self.scope_token.clone();
        Ok(agent)
    }

    /// The mode the row's agent ran in.
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

/// Record `pending` for the space `id`.
pub fn record(tx: &Transaction<'_>, id: &SpaceId, pending: &Pending) -> Result<()> {
    tx.execute(
        "INSERT INTO space_conversions(space_id,record) VALUES (?1,?2)",
        params![id, serde_json::to_string(pending)?],
    )?;
    Ok(())
}

/// The conversion of the space `id` whose agent has yet to start in the
/// new place.
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

/// Every conversion whose agent has yet to start in the new place.
pub fn all_pending(db: &Connection) -> Result<Vec<(SpaceId, Pending)>> {
    db.prepare("SELECT space_id,record FROM space_conversions")?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .map(|row| {
            let (id, record) = row?;
            Ok((id, serde_json::from_str(&record)?))
        })
        .collect()
}

/// Record `operation` as the one preparing the workspace `id` a chat
/// became, whose end its agent's restart there waits for.
pub fn record_preparation(db: &Connection, id: &SpaceId, operation: &str) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.preparation',?2)
         WHERE space_id=?1 AND json_extract(record,'$.to')='workspace'",
        params![id, operation],
    )?;
    Ok(())
}

/// Keep that the preparation `operation` of the workspace `id` a chat became
/// ended, with why its post-setup hook failed when it did. An end reported
/// before the conversion recorded its preparation, which starts before it
/// does, records that preparation too.
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

/// Hold the conversion of the space `id` into a chat, or to another agent
/// tool, whose agent a stop kept down: the agent that carries on stays
/// stopped once the conversion is made.
pub fn hold(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_conversions SET record=json_set(record,'$.held',json('true'))
         WHERE space_id=?1 AND json_extract(record,'$.to') IN ('chat','agent')",
        [id],
    )?;
    Ok(())
}

/// Forget the conversion of the space `id`, once its agent started in the
/// new place.
pub fn forget(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_conversions WHERE space_id=?1", [id])?;
    Ok(())
}

/// Record that the workspace `id` becomes `chat` once its agent's turn ends,
/// while the workspace is ready and that agent's execution runs, as a stop,
/// which first reserves the workspace, has not ended it.
pub fn record_chat_conversion(
    tx: &Transaction<'_>,
    id: &SpaceId,
    chat: ConvertedChat,
) -> Result<()> {
    let ready = crate::store::exists(
        tx,
        "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
        params![id, crate::state::WorkspaceState::Ready],
    )?;
    let running = crate::store::execution_state(tx, &chat.execution)?
        == Some((id.to_owned(), crate::state::ExecutionState::Running));
    anyhow::ensure!(
        ready && running,
        "the workspace's agent was stopped meanwhile; it stays a workspace"
    );
    let pending = Pending::Chat {
        chat: Box::new(chat),
        held: false,
    };
    record(tx, id, &pending)
}

/// Record that `taking` takes the work of the space `id` over once the turn
/// of the agent `execution` runs ends, while that agent's execution runs.
pub fn record_agent_switch(
    tx: &Transaction<'_>,
    id: &SpaceId,
    execution: ExecutionId,
    taking: Taking,
) -> Result<()> {
    let running = crate::store::execution_state(tx, &execution)?
        == Some((id.to_owned(), crate::state::ExecutionState::Running));
    ensure!(running, "the agent was stopped meanwhile; convert it again");
    let pending = Pending::Agent {
        execution,
        taking: Box::new(taking),
        held: false,
    };
    record(tx, id, &pending)
}

/// Replace the chat `id`, whose workspace row was just inserted under its
/// ID, with that workspace, whose agent runs in `mode` with the permissions
/// `narrowing` gives it, unless a stop saved the chat meanwhile: its
/// threads, approval requests and unread messages become the workspace's,
/// and its agent's scope token, which it keeps, acts for the workspace until
/// the agent restarts there.
/// Returns the row the chat had and the execution its agent runs as.
pub fn replace_chat(
    tx: &Transaction<'_>,
    id: &SpaceId,
    mode: &ModeName,
    narrowing: &PermissionsConfig,
) -> Result<(ChatRow, ExecutionId)> {
    // A chat's executions run its agent and nothing else.
    let chat = crate::store::exists(tx, "SELECT 1 FROM chats WHERE id=?1", [id])?;
    let execution = (crate::store::newest_execution(tx, id)?)
        .filter(|_| chat)
        .context("the chat was stopped meanwhile; it stays a chat")?;
    let row = chat_row(tx, id)?;
    forget_completion(tx, id)?;
    spaces::convert(tx, id, SpaceKind::Workspace, mode, narrowing)?;
    tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
    Ok((row, execution))
}

/// Forget the conversion of the chat that became the workspace `id`, whose
/// agent, which carried the scope `token`, restarted there under a scope of
/// its own. The tokens the chat issued go too, as a workspace's exported
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

/// Give back the chat `row`, which the workspace `id` replaced, and remove
/// that workspace's row: its completion, threads, approval requests and
/// unread messages return to the chat, and of its scope tokens only the
/// chat's own stays.
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

/// The stopped chat a workspace becomes: its agent, the execution it ran
/// as, whose session it carries on, the place it ran in, its mode and the
/// permissions given to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertedChat {
    pub agent: Agent,
    pub execution: ExecutionId,
    pub origin: Option<Origin>,
    pub mode: ModeName,
    pub narrowing: PermissionsConfig,
}

/// Replace the workspace `id` with the stopped chat `chat` under its ID,
/// number and name, which restores in its repository's checkout: its
/// threads, unread messages and requests to take actions become the
/// chat's, and what only a workspace has goes with its row. Returns the requests the workspace
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

/// Forget the completion of the space `id`, which ends with the kind it
/// had: its record, which a conversion keeps, does not take it with it.
fn forget_completion(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM space_completion WHERE space_id=?1", [id])?;
    Ok(())
}

/// The row of the chat `id`, with its space's.
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

/// Make the space `id` the chat `row` again, as it was.
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
