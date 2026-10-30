//! What the daemon knows of workspaces' agents: running, queued, or stopped
//! with a session to restore.
use std::collections::HashMap;

use anyhow::Result;
use shoal_client::records as api;

use crate::{
    daemon::{
        store::{self, AgentExecution},
        workspace::Manager,
    },
    execution::recovery,
    model::{QueueEntry, Workspace},
    state::{AgentState, ExecutionState, QueueState},
};

/// What the daemon knows of the agents of every workspace.
pub(super) struct Agents {
    /// The agent execution saying most about each workspace's agent, by
    /// workspace ID.
    executions: HashMap<String, AgentExecution>,
    /// Queued launches and restores, by the workspace they start in.
    queued: HashMap<String, QueueEntry>,
    /// The first stopped agent with a session to restore, by workspace ID.
    stopped: HashMap<String, String>,
}

impl Agents {
    /// The workspace's agent: running or unconfirmed, else one reporting its
    /// turns untracked, else queued, else stopped with a session to restore
    /// or a wrapper waiting to restore it.
    pub(super) fn status(&self, workspace: &Workspace) -> Option<api::AgentStatus> {
        let execution = self.executions.get(&workspace.id);
        if let Some(execution) = execution
            .filter(|execution| execution.started || execution.state == ExecutionState::Unknown)
        {
            return Some(running(execution, workspace));
        }
        // An agent started from an exported workspace environment reports
        // its turns without a tracked execution.
        if let (None, Some(status)) = (execution, workspace.agent_state) {
            return Some(api::AgentStatus {
                state: agent_state(status.state),
                agent: None,
                since: Some(status.since),
            });
        }
        if let Some(entry) = self.queued.get(&workspace.id) {
            return Some(api::AgentStatus {
                state: match entry.state {
                    QueueState::Queued => api::AgentState::Queued,
                    QueueState::Starting => api::AgentState::Starting,
                },
                agent: Some(entry.agent.clone()),
                since: Some(entry.created_at),
            });
        }
        let stopped = self.stopped.get(&workspace.id);
        stopped
            .or(execution.map(|execution| &execution.agent))
            .map(|agent| api::AgentStatus {
                state: api::AgentState::Stopped,
                agent: Some(agent.clone()),
                since: None,
            })
    }
}

/// The agent `execution` runs, or may still run after a restart.
fn running(execution: &AgentExecution, workspace: &Workspace) -> api::AgentStatus {
    let (state, since) = match (execution.state, workspace.agent_state) {
        (ExecutionState::Unknown, _) => (api::AgentState::Unconfirmed, None),
        (ExecutionState::Running, Some(status)) => (agent_state(status.state), Some(status.since)),
        (ExecutionState::Running, None) => (api::AgentState::Running, None),
    };
    api::AgentStatus {
        state,
        agent: Some(execution.agent.clone()),
        since,
    }
}

fn agent_state(state: AgentState) -> api::AgentState {
    match state {
        AgentState::Working => api::AgentState::Working,
        AgentState::Waiting => api::AgentState::Waiting,
        AgentState::Idle => api::AgentState::Idle,
    }
}

/// The running, queued and stopped agents of `workspaces`.
pub(super) async fn load(manager: &Manager, workspaces: &[Workspace]) -> Result<Agents> {
    Ok(Agents {
        executions: manager.store.run(store::agent_executions).await?,
        queued: queued(manager).await?,
        stopped: stopped(manager, workspaces)?,
    })
}

/// The first queued launch or restore of each workspace.
async fn queued(manager: &Manager) -> Result<HashMap<String, QueueEntry>> {
    let mut queued = HashMap::new();
    for entry in manager.queue_entries().await? {
        if let Some(workspace) = entry.workspace() {
            queued.entry(workspace.to_owned()).or_insert(entry);
        }
    }
    Ok(queued)
}

/// The agent each of `workspaces` can resume: its first stopped agent,
/// which protection or a restart stopped, or else its first last session,
/// which a normal stop or exit keeps.
fn stopped(manager: &Manager, workspaces: &[Workspace]) -> Result<HashMap<String, String>> {
    let mut stopped = HashMap::new();
    for workspace in workspaces {
        let paths = &manager.paths;
        let agent = match recovery::stopped_agents(paths, &workspace.id)?
            .into_iter()
            .next()
        {
            Some(agent) => Some(agent.record.agent),
            None => recovery::LastSession::all(paths, &workspace.id)?
                .into_iter()
                .next()
                .map(|last| last.agent),
        };
        if let Some(agent) = agent {
            stopped.insert(workspace.id.clone(), agent);
        }
    }
    Ok(stopped)
}
