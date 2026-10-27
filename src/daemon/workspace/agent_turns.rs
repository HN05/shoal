//! What each tracked agent is doing, as protection reads it: the turn state
//! its hooks last reported, where an idle agent that still runs work outside
//! its process group counts as working, since that work may start its next
//! turn.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use super::Manager;
use crate::{
    daemon::store,
    model::Execution,
    process::{execution::Processes, identity as process},
    state::AgentState,
};

/// An agent's turn state and when it began, in Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Turn {
    pub state: AgentState,
    pub since: i64,
}

impl Turn {
    pub(crate) fn idle(&self) -> bool {
        self.state == AgentState::Idle
    }
}

/// The turn each execution last reported, with the records of the idle ones.
fn reported(db: &Connection) -> Result<(HashMap<String, Turn>, Vec<Execution>)> {
    let turns: HashMap<String, Turn> = db
        .prepare(
            "SELECT execution_id,state,since FROM workspace_agent_state
             WHERE execution_id IS NOT NULL",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Turn {
                    state: row.get(1)?,
                    since: row.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut idle = Vec::new();
    for (id, _) in turns.iter().filter(|(_, turn)| turn.idle()) {
        idle.extend(store::find_execution(db, id)?);
    }
    Ok((turns, idle))
}

impl Manager {
    /// The turn of each execution that reported one.
    pub(crate) async fn agent_turns(&self) -> Result<HashMap<String, Turn>> {
        let (mut turns, idle) = self.store.run(|db| reported(db)).await?;
        if idle.is_empty() {
            return Ok(turns);
        }
        let scan =
            process::scan(idle.iter().map(|execution| execution.id.clone()).collect()).await?;
        for execution in idle {
            if Processes::runs_outside_group(&execution, &scan)
                && let Some(turn) = turns.get_mut(&execution.id)
            {
                turn.state = AgentState::Working;
            }
        }
        Ok(turns)
    }
}
