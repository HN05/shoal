//! The executions spaces record, and the reads of them that the subsystems
//! tracking agents share.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row};
use shoal_core::process::identity::Identity;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::agent::{Agent, ModelChoice};
use shoal_protocol::model::Execution;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::state::ExecutionState;

use crate::sql::json_column;

pub const EXECUTION_COLUMNS: &str = "id,space_id,state,wrapper,child,group_id,origin_host,
    origin_place,origin_instance,origin,agent,model,effort,mode,narrowing";

pub fn execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    Ok(Execution {
        id: row.get("id")?,
        space_id: row.get("space_id")?,
        state: row.get("state")?,
        wrapper: json_column(row, "wrapper")?,
        child: json_column(row, "child")?,
        group_id: row.get("group_id")?,
        origin: crate::origins::origin(row)?,
        agent: row.get("agent")?,
        model: ModelChoice {
            model: row.get("model")?,
            effort: row.get("effort")?,
        },
        mode: row.get("mode")?,
        narrowing: json_column(row, "narrowing")?.unwrap_or_default(),
        stopped: None,
    })
}

fn executions_query() -> String {
    format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE space_id=?1")
}

pub fn find_execution(db: &Connection, id: &ExecutionId) -> Result<Option<Execution>> {
    Ok(db
        .query_row(
            &format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE id=?1"),
            [id],
            execution,
        )
        .optional()?)
}

/// The scope token the commands of the execution `id` act under, while it
/// exists and keeps one.
pub fn scope_token(db: &Connection, id: &ExecutionId) -> Result<Option<ScopeToken>> {
    let token = db
        .query_row(
            "SELECT scope_token FROM executions WHERE id=?1",
            [id],
            |row| row.get::<_, Option<ScopeToken>>(0),
        )
        .optional()?;
    Ok(token.flatten())
}

/// The executions of the space `space_id`.
pub fn executions(db: &Connection, space_id: &SpaceId) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&executions_query())?
        .query_map([space_id], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The wrappers of the recorded executions that run an agent.
pub fn agent_wrappers(db: &Connection) -> Result<Vec<Identity>> {
    let mut statement = db.prepare(
        "SELECT wrapper FROM executions WHERE agent IS NOT NULL AND wrapper IS NOT NULL",
    )?;
    let wrappers = statement.query_map([], |row| json_column(row, "wrapper"))?;
    Ok(wrappers
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// A recorded execution that runs an agent.
pub struct AgentExecution {
    pub agent: Agent,
    pub state: ExecutionState,
    /// Whether its wrapper runs the agent now, rather than waiting to start
    /// it or to restore one protection stopped.
    pub started: bool,
}

impl AgentExecution {
    /// How much the execution says about its space's agent: one that
    /// runs, then one not known to, then a wrapper without one.
    fn weight(&self) -> u8 {
        match (self.state, self.started) {
            (ExecutionState::Running, true) => 2,
            (ExecutionState::Unknown, _) => 1,
            (ExecutionState::Running, false) => 0,
        }
    }
}

/// The agent execution saying most about each space's agent, by space ID.
pub fn agent_executions(db: &mut Connection) -> Result<HashMap<SpaceId, AgentExecution>> {
    let mut agents: HashMap<SpaceId, AgentExecution> = HashMap::new();
    let mut statement = db.prepare(
        "SELECT space_id,agent,state,child IS NOT NULL FROM executions WHERE agent IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        let execution = AgentExecution {
            agent: row.get(1)?,
            state: row.get(2)?,
            started: row.get(3)?,
        };
        Ok((row.get::<_, SpaceId>(0)?, execution))
    })?;
    for row in rows {
        let (space, execution) = row?;
        match agents.get(&space) {
            Some(known) if known.weight() >= execution.weight() => {}
            _ => {
                agents.insert(space, execution);
            }
        }
    }
    Ok(agents)
}

pub fn all_executions(db: &Connection) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!("SELECT {EXECUTION_COLUMNS} FROM executions"))?
        .query_map([], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Each recorded execution's ID, with the space it runs in.
pub fn execution_spaces(db: &Connection) -> Result<Vec<(ExecutionId, SpaceId)>> {
    Ok(db
        .prepare("SELECT id,space_id FROM executions ORDER BY rowid")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
