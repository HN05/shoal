//! What each tracked agent is doing, as protection reads it: the turn state
//! its hooks last reported, where an idle agent that still runs a subagent,
//! or work outside its process group, counts as working, since that work
//! may start its next turn.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::{
    Agents,
    model::Execution,
    model::RunningAgent,
    process::{execution::Processes, identity as process},
    state::AgentState,
    subagents,
    time::UnixTime,
};

/// An agent's turn state, when it began and the turn generation of its
/// space it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub state: AgentState,
    pub since: UnixTime,
    pub generation: i64,
}

impl Turn {
    pub fn idle(&self) -> bool {
        self.state == AgentState::Idle
    }
}

/// The turn each execution last reported, an idle one working while it
/// runs a subagent at `now`, with the records of the idle ones.
fn reported(
    db: &Connection,
    now: UnixTime,
) -> Result<(HashMap<ExecutionId, Turn>, Vec<Execution>)> {
    let mut turns = last_turns(db)?;
    count_subagents(&mut turns, subagents::running(db, now)?);
    let mut idle = Vec::new();
    for (id, _) in turns.iter().filter(|(_, turn)| turn.idle()) {
        idle.extend(shoal_store::find_execution(db, id)?);
    }
    Ok((turns, idle))
}

/// The turn each execution last reported.
fn last_turns(db: &Connection) -> Result<HashMap<ExecutionId, Turn>> {
    Ok(db
        .prepare(
            "SELECT execution_id,state,since,turn FROM space_agent_state
             WHERE execution_id IS NOT NULL",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Turn {
                    state: row.get(1)?,
                    since: row.get(2)?,
                    generation: row.get(3)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Count the idle `turns` of the executions that run a subagent, as
/// `running` names them, as working.
fn count_subagents(turns: &mut HashMap<ExecutionId, Turn>, running: HashSet<ExecutionId>) {
    for id in running {
        if let Some(turn) = turns.get_mut(&id).filter(|turn| turn.idle()) {
            turn.state = AgentState::Working;
        }
    }
}

impl Agents {
    /// The turn each execution reported last, without the process scan
    /// that tells an idle one with work in flight.
    pub async fn reported_turns(&self) -> Result<HashMap<ExecutionId, Turn>> {
        Ok(self.store.read(|db| reported(db, UnixTime::now())).await?.0)
    }

    /// The turn of each execution that reported one.
    pub async fn agent_turns(&self) -> Result<HashMap<ExecutionId, Turn>> {
        let (mut turns, idle) = self.store.read(|db| reported(db, UnixTime::now())).await?;
        if idle.is_empty() {
            return Ok(turns);
        }
        let scan =
            process::scan(idle.iter().map(|execution| execution.id.clone()).collect()).await?;
        let agents = self.agent_trees().await?;
        for execution in idle {
            if Processes::runs_outside_group(&execution, &scan, &agents)
                && let Some(turn) = turns.get_mut(&execution.id)
            {
                turn.state = AgentState::Working;
            }
        }
        Ok(turns)
    }
}

impl Agents {
    /// Whether the agent of the space `id` ended its turn: it reported
    /// itself idle, or no agent `runs` matches runs any longer.
    pub async fn turn_ended(
        &self,
        id: &SpaceId,
        runs: impl Fn(&RunningAgent) -> bool,
    ) -> Result<bool> {
        let space = id.to_owned();
        let state = self
            .store
            .read(move |db| crate::turn_state::current(db, &space))
            .await?;
        if state.is_some_and(|status| status.state == AgentState::Idle) {
            return Ok(true);
        }
        let running = self.running_agents().await?;
        Ok(!running.iter().any(runs))
    }
}
