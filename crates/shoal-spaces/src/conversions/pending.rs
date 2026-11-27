//! The record of a conversion whose agent has yet to start in the new
//! place.
use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::handoff::Taking;
use super::record::ChatRow;
use crate::{
    agent::Agent,
    config::{modes::ModeName, permissions::PermissionsConfig},
    sessions::Origin,
    state::ConversionTarget,
};

/// The agent still runs where the space was, until its turn ends.
#[derive(Debug, Clone)]
pub enum Pending {
    Workspace(ToWorkspace),
    Chat(ToChat),
    Agent(ToAgent),
}

/// A chat that became a workspace. The row the chat had gives its running
/// agent, and gives the chat back should the workspace's worktree fail. The
/// agent runs as `execution`, whose session files hold its session and
/// under which its last session in the workspace is kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToWorkspace {
    pub mode: ModeName,
    pub chat: ChatRow,
    pub execution: ExecutionId,
    /// The agent waits for this preparation to end, post-setup hook
    /// included. Its own column holds it.
    #[serde(skip)]
    pub preparation: Option<String>,
    /// Kept beyond the operation's own record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prepared: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Carrying::is_default")]
    pub carrying: Carrying,
}

/// The agent's details outlive its execution, as when the agent exits
/// first. A stop that keeps the workspace's agents down until `shoal
/// resume` `held` it: the workspace still becomes the chat, which stays
/// stopped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToChat {
    pub chat: Box<ConvertedChat>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    #[serde(default, skip_serializing_if = "Carrying::is_default")]
    pub carrying: Carrying,
}

/// Runs once the turn of the agent `execution` runs ends. A stop that keeps
/// that agent down until `shoal resume` `held` it: `taking` then waits
/// stopped too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToAgent {
    pub execution: ExecutionId,
    pub taking: Box<Taking>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

impl Pending {
    /// Rather than leave it for `shoal resume`: true unless a stop held it or
    /// the new workspace's post-setup hook failed.
    pub fn restarts_agent(&self) -> bool {
        match self {
            Self::Workspace(to) => to.hook_failure.is_none(),
            Self::Chat(ToChat { held, .. }) | Self::Agent(ToAgent { held, .. }) => !held,
        }
    }

    /// Its row's columns: the kind of conversion, the preparation the agent
    /// waits for and the record of the rest.
    fn columns(&self) -> Result<(ConversionTarget, Option<&str>, String)> {
        Ok(match self {
            Self::Workspace(to) => (
                ConversionTarget::Workspace,
                to.preparation.as_deref(),
                serde_json::to_string(to)?,
            ),
            Self::Chat(to) => (ConversionTarget::Chat, None, serde_json::to_string(to)?),
            Self::Agent(to) => (ConversionTarget::Agent, None, serde_json::to_string(to)?),
        })
    }

    fn from_columns(
        target: ConversionTarget,
        preparation: Option<String>,
        record: &str,
    ) -> Result<Self> {
        Ok(match target {
            ConversionTarget::Workspace => Self::Workspace(ToWorkspace {
                preparation,
                ..serde_json::from_str(record)?
            }),
            ConversionTarget::Chat => Self::Chat(serde_json::from_str(record)?),
            ConversionTarget::Agent => Self::Agent(serde_json::from_str(record)?),
        })
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
    let (target, preparation, record) = pending.columns()?;
    tx.execute(
        "INSERT INTO space_conversions(space_id,target,preparation,record)
         VALUES (?1,?2,?3,?4)",
        params![id, target, preparation, record],
    )?;
    Ok(())
}

/// Writes `id`'s conversion back when `edit` changed it.
fn change(
    tx: &Transaction<'_>,
    id: &SpaceId,
    edit: impl FnOnce(&mut Pending) -> bool,
) -> Result<()> {
    let Some(mut pending) = pending(tx, id)? else {
        return Ok(());
    };
    if !edit(&mut pending) {
        return Ok(());
    }
    let (target, preparation, record) = pending.columns()?;
    tx.execute(
        "UPDATE space_conversions SET target=?2,preparation=?3,record=?4 WHERE space_id=?1",
        params![id, target, preparation, record],
    )?;
    Ok(())
}

const COLUMNS: &str = "space_id,target,preparation,record";

fn row(row: &rusqlite::Row<'_>) -> Result<(SpaceId, Pending)> {
    let id: SpaceId = row.get(0)?;
    let record: String = row.get(3)?;
    let pending = Pending::from_columns(row.get(1)?, row.get(2)?, &record)
        .with_context(|| format!("space_conversions row {id}"))?;
    Ok((id, pending))
}

/// One whose agent has yet to start in the new place.
pub fn pending(db: &Connection, id: &SpaceId) -> Result<Option<Pending>> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM space_conversions WHERE space_id=?1"
    ))?;
    let mut rows = statement.query([id])?;
    rows.next()?.map(|found| Ok(row(found)?.1)).transpose()
}

/// Those whose agent has yet to start in the new place.
pub fn all_pending(db: &Connection) -> Result<Vec<(SpaceId, Pending)>> {
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM space_conversions"))?;
    let mut rows = statement.query([])?;
    let mut all = Vec::new();
    while let Some(found) = rows.next()? {
        all.push(row(found)?);
    }
    Ok(all)
}

/// The agent's restart in the workspace waits for its end.
pub fn record_preparation(tx: &Transaction<'_>, id: &SpaceId, operation: &str) -> Result<()> {
    change(tx, id, |pending| match pending {
        Pending::Workspace(to) => {
            to.preparation = Some(operation.to_owned());
            true
        }
        Pending::Chat(_) | Pending::Agent(_) => false,
    })
}

/// Records why its post-setup hook failed, when it did. An end reported
/// before the conversion recorded its preparation, which starts first,
/// records that preparation too.
pub fn record_prepared(
    tx: &Transaction<'_>,
    id: &SpaceId,
    operation: &str,
    hook_failure: Option<&str>,
) -> Result<()> {
    change(tx, id, |pending| match pending {
        Pending::Workspace(to)
            if to
                .preparation
                .as_deref()
                .is_none_or(|recorded| recorded == operation) =>
        {
            to.preparation = Some(operation.to_owned());
            to.prepared = true;
            to.hook_failure = hook_failure.map(str::to_owned);
            true
        }
        Pending::Workspace(_) | Pending::Chat(_) | Pending::Agent(_) => false,
    })
}

/// The agent that carries on stays stopped once the conversion is made.
pub fn hold(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    change(tx, id, |pending| match pending {
        Pending::Chat(ToChat { held, .. }) | Pending::Agent(ToAgent { held, .. }) => {
            *held = true;
            true
        }
        Pending::Workspace(_) => false,
    })
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
    let pending = Pending::Chat(ToChat {
        chat: Box::new(chat),
        held: false,
        carrying,
    });
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
    let pending = Pending::Agent(ToAgent {
        execution,
        taking: Box::new(taking),
        held: false,
    });
    record(tx, id, &pending)
}

/// No agent starts there until the space's sessions are copied.
pub fn record_stopped_switch(
    tx: &Transaction<'_>,
    id: &SpaceId,
    execution: ExecutionId,
    taking: Taking,
) -> Result<()> {
    let pending = Pending::Agent(ToAgent {
        execution,
        taking: Box::new(taking),
        held: false,
    });
    record(tx, id, &pending)
}

#[cfg(test)]
mod tests;
