//! Workspaces whose coordinator does their work itself: those in which no
//! agent of their own has started, as a split child before `shoal start`
//! gives it one, which the workspace's unrecorded mode tells. The
//! coordinator's subagents work there from inside its agent, reporting
//! under its scope from its directory, so Shoal cannot tell which such
//! workspace a subagent works in; each counts as its coordinator's work.
use anyhow::Result;
use rusqlite::Connection;

use super::{Manager, TrackedAgents, agent_state, tree};
use crate::{daemon::store, model::Workspace, state::AgentState};

impl Manager {
    /// Fill in, for each of `workspaces` whose coordinator does its work,
    /// the turn state that coordinator's agent reports.
    pub(super) async fn load_coordinator_states(&self, workspaces: &mut [Workspace]) -> Result<()> {
        let agents = self.tracked_agents().await;
        let ids: Vec<String> = workspaces.iter().map(|w| w.id.clone()).collect();
        let states = self
            .store
            .run(move |db| {
                ids.iter()
                    .map(|id| coordinator_state(db, id, &agents))
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        for (workspace, state) in workspaces.iter_mut().zip(states) {
            workspace.coordinator_state = state;
        }
        Ok(())
    }
}

/// The turn state the agent of the coordinator that does the work of
/// workspace `id` reports.
fn coordinator_state(
    db: &Connection,
    id: &str,
    agents: &TrackedAgents,
) -> Result<Option<AgentState>> {
    let Some(coordinator) = working_coordinator(db, id, agents)? else {
        return Ok(None);
    };
    Ok(agent_state::current(db, &coordinator)?.map(|status| status.state))
}

/// The coordinator of workspace `id` while no agent of its own has started
/// there.
fn working_coordinator(
    db: &Connection,
    id: &str,
    agents: &TrackedAgents,
) -> Result<Option<String>> {
    let agentless = store::exists(
        db,
        "SELECT 1 FROM workspaces WHERE id=?1 AND mode IS NULL",
        [id],
    )?;
    if !agentless {
        return Ok(None);
    }
    tree::coordinator_of(db, id, agents)
}

#[cfg(test)]
mod tests;
