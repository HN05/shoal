//! What the daemon knows of spaces' agents: running, queued, or stopped
//! with a session to restore.
use crate::execution::recovery::LastSessionExt;
use shoal_ids::space_ids::SpaceId;
use std::collections::HashMap;

use anyhow::Result;

use crate::{
    daemon::{
        store::{self, AgentExecution},
        workspace::Manager,
    },
    execution::recovery,
    model::{QueueEntry, Space},
    state::{AgentState, ExecutionState, QueueState},
};
use shoal_protocol::subscriptions::records::{ActivityState, AgentActivity};

/// What the daemon knows of the agents of every space.
pub(super) struct Agents {
    /// The agent execution saying most about each space's agent, by space
    /// ID.
    executions: HashMap<SpaceId, AgentExecution>,
    /// Queued launches and restores, by the space they start in.
    queued: HashMap<SpaceId, QueueEntry>,
    /// The first stopped agent with a session to restore, by space ID.
    stopped: HashMap<SpaceId, crate::agent::Agent>,
}

impl Agents {
    /// The space's agent: running or unconfirmed, else one reporting its
    /// turns untracked, else queued, else stopped with a session to restore
    /// or a wrapper waiting to restore it.
    pub(super) fn activity(&self, space: &Space) -> Option<AgentActivity> {
        let execution = self.executions.get(&space.id);
        if let Some(execution) = execution
            .filter(|execution| execution.started || execution.state == ExecutionState::Unknown)
        {
            return Some(running(execution, space));
        }
        // An agent started from an exported workspace environment reports
        // its turns without a tracked execution.
        if let (None, Some(status)) = (execution, space.agent_state) {
            return Some(AgentActivity {
                state: turn(status.state),
                agent: None,
                since: Some(status.since),
            });
        }
        if let Some(entry) = self.queued.get(&space.id) {
            return Some(AgentActivity {
                state: match entry.state {
                    QueueState::Queued => ActivityState::Queued,
                    QueueState::Starting => ActivityState::Starting,
                    QueueState::Failed => ActivityState::Failed,
                },
                agent: Some(entry.agent.clone()),
                since: Some(entry.created_at),
            });
        }
        let stopped = self.stopped.get(&space.id);
        stopped
            .or(execution.map(|execution| &execution.agent))
            .map(|agent| AgentActivity {
                state: ActivityState::Stopped,
                agent: Some(agent.clone()),
                since: None,
            })
    }
}

/// The agent `execution` runs, or may still run after a restart.
fn running(execution: &AgentExecution, space: &Space) -> AgentActivity {
    let (state, since) = match (execution.state, space.agent_state) {
        (ExecutionState::Unknown, _) => (ActivityState::Unconfirmed, None),
        (ExecutionState::Running, Some(status)) => (turn(status.state), Some(status.since)),
        (ExecutionState::Running, None) => (ActivityState::Running, None),
    };
    AgentActivity {
        state,
        agent: Some(execution.agent.clone()),
        since,
    }
}

fn turn(state: AgentState) -> ActivityState {
    match state {
        AgentState::Working => ActivityState::Working,
        AgentState::Waiting => ActivityState::Waiting,
        AgentState::Idle => ActivityState::Idle,
    }
}

/// The running, queued and stopped agents of `spaces`.
pub(super) async fn load(manager: &Manager, spaces: &[Space]) -> Result<Agents> {
    Ok(Agents {
        executions: manager.store.run(store::agent_executions).await?,
        queued: queued(manager).await?,
        stopped: stopped(manager, spaces)?,
    })
}

/// The first queued launch or restore of each space.
async fn queued(manager: &Manager) -> Result<HashMap<SpaceId, QueueEntry>> {
    let mut queued = HashMap::new();
    for entry in manager.queue_entries().await? {
        if let Some(space) = entry.space() {
            queued.entry(space.to_owned()).or_insert(entry);
        }
    }
    Ok(queued)
}

/// The agent each of `spaces` can resume: its first stopped agent,
/// which protection or a restart stopped, or else its first last session,
/// which a normal stop or exit keeps.
fn stopped(manager: &Manager, spaces: &[Space]) -> Result<HashMap<SpaceId, crate::agent::Agent>> {
    let mut stopped = HashMap::new();
    for space in spaces {
        let paths = &manager.paths;
        let agent = match recovery::stopped_agents(paths, &space.id)?
            .into_iter()
            .next()
        {
            Some(agent) => Some(agent.record.agent),
            None => recovery::LastSession::all(paths, &space.id)?
                .into_iter()
                .next()
                .map(|last| last.agent),
        };
        if let Some(agent) = agent {
            stopped.insert(space.id.clone(), agent);
        }
    }
    Ok(stopped)
}
