//! The record of a conversion whose agent has yet to start in the new
//! place.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::handoff::Taking;
use super::record::ChatRow;
use crate::{
    agent::Agent,
    config::{modes::ModeName, permissions::PermissionsConfig},
    sessions::Origin,
};

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
