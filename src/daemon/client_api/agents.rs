//! What the daemon knows of workspaces' agents: running, queued, or stopped
//! with a session to restore.
use std::collections::HashMap;

use anyhow::Result;
use shoal_client::records as api;

use crate::{
    daemon::workspace::Manager,
    execution::recovery,
    model::{QueueEntry, Workspace},
    state::{AgentState, QueueState},
};

/// What the daemon knows of the agents of every workspace.
pub(super) struct Agents {
    /// The agent each workspace runs, by workspace ID; `None` for an
    /// execution whose agent is not recorded.
    running: HashMap<String, Option<String>>,
    /// Queued launches and restores, by the workspace they start in.
    queued: HashMap<String, QueueEntry>,
    /// The first stopped agent with a session to restore, by workspace ID.
    stopped: HashMap<String, String>,
}

impl Agents {
    /// The workspace's agent: running, else queued, else stopped.
    pub(super) fn status(&self, workspace: &Workspace) -> Option<api::AgentStatus> {
        if let Some(agent) = self.running.get(&workspace.id) {
            let (state, since) = match workspace.agent_state {
                Some(status) => (agent_state(status.state), Some(status.since)),
                None => (api::AgentState::Running, None),
            };
            return Some(api::AgentStatus {
                state,
                agent: agent.clone(),
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
    let running = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT workspace_id,agent FROM executions WHERE agent IS NOT NULL")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<HashMap<String, Option<String>>>>()?)
        })
        .await?;
    let mut queued = HashMap::new();
    for entry in manager.queue_entries().await? {
        if let Some(workspace) = entry.workspace() {
            queued.entry(workspace.to_owned()).or_insert(entry);
        }
    }
    let mut stopped = HashMap::new();
    for workspace in workspaces {
        if let Some(agent) = recovery::stopped_agents(&manager.paths, &workspace.id)?.first() {
            stopped.insert(workspace.id.clone(), agent.record.agent.clone());
        }
    }
    Ok(Agents {
        running,
        queued,
        stopped,
    })
}
