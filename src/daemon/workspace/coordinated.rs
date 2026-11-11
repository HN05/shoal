//! Spaces whose coordinator does their work itself: workspaces in which no
//! agent of their own has started, as a split child before `shoal start`
//! gives it one, which the workspace's unrecorded mode tells, and chats
//! whose agent stopped. The coordinator's subagents work there from inside
//! its agent, reporting under its scope from its directory, so Shoal cannot
//! tell which such workspace a subagent works in; each counts as its
//! coordinator's work, and a stopped chat's work is left to it.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::Connection;

use super::{Manager, TrackedAgents, agent_state, tree};
use crate::{daemon::store, model::Workspace, state::AgentState};

impl Manager {
    /// Fill in, for each of `workspaces` whose coordinator does its work,
    /// the turn state that coordinator's agent reports.
    pub(super) async fn load_coordinator_states(&self, workspaces: &mut [Workspace]) -> Result<()> {
        let agents = self.tracked_agents().await;
        let ids: Vec<SpaceId> = workspaces.iter().map(|w| w.id.clone()).collect();
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

    /// Whether the agent of the coordinator that does the work of the space
    /// `id` runs, in any turn state, since its subagents may work on past
    /// its turn.
    pub(super) async fn coordinator_runs(&self, id: &SpaceId) -> Result<bool> {
        let agents = self.tracked_agents().await;
        let id = id.to_owned();
        self.store
            .run(move |db| match working_coordinator(db, &id, &agents)? {
                Some(coordinator) => tree::agent_runs(db, &coordinator, &agents),
                None => Ok(false),
            })
            .await
    }
}

/// The turn state the agent of the coordinator that does the work of
/// workspace `id` reports.
fn coordinator_state(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<AgentState>> {
    let Some(coordinator) = working_coordinator(db, id, agents)? else {
        return Ok(None);
    };
    Ok(agent_state::current(db, &coordinator)?.map(|status| status.state))
}

/// The coordinator of the space `id` while no agent of its own works there:
/// none has started in the workspace, or the chat's has stopped.
fn working_coordinator(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<SpaceId>> {
    let agentless = store::exists(
        db,
        "SELECT 1 FROM workspaces WHERE id=?1 AND mode IS NULL
         UNION ALL SELECT 1 FROM chats WHERE id=?1 AND restore IS NOT NULL",
        [id],
    )?;
    if !agentless {
        return Ok(None);
    }
    tree::coordinator_of(db, id, agents)
}

#[cfg(test)]
mod tests;
