//! What the daemon knows of workspaces' agents: running, queued, or stopped
//! with a session to restore.
use std::collections::HashMap;

use anyhow::Result;
use shoal_client::records as api;

use crate::{
    daemon::{store, workspace::Manager},
    execution::recovery,
    model::{QueueEntry, Workspace},
    state::{AgentState, ExecutionState, QueueState},
};

/// What the daemon knows of the agents of every workspace.
pub(super) struct Agents {
    /// The agent each workspace's recorded execution runs, by workspace ID,
    /// and whether it is known to run.
    running: HashMap<String, (String, ExecutionState)>,
    /// Queued launches and restores, by the workspace they start in.
    queued: HashMap<String, QueueEntry>,
    /// The first stopped agent with a session to restore, by workspace ID.
    stopped: HashMap<String, String>,
}

impl Agents {
    /// The workspace's agent: running, else queued, else stopped.
    pub(super) fn status(&self, workspace: &Workspace) -> Option<api::AgentStatus> {
        if let Some((agent, execution)) = self.running.get(&workspace.id) {
            let (state, since) = match (execution, workspace.agent_state) {
                (ExecutionState::Unknown, _) => (api::AgentState::Unconfirmed, None),
                (ExecutionState::Running, Some(status)) => {
                    (agent_state(status.state), Some(status.since))
                }
                (ExecutionState::Running, None) => (api::AgentState::Running, None),
            };
            return Some(api::AgentStatus {
                state,
                agent: Some(agent.clone()),
                since,
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
        self.stopped
            .get(&workspace.id)
            .map(|agent| api::AgentStatus {
                state: api::AgentState::Stopped,
                agent: Some(agent.clone()),
                since: None,
            })
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
        running: manager.store.run(store::agent_executions).await?,
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
