//! Spaces whose coordinator does their work: workspaces where no agent of
//! their own has started, and chats whose agent stopped. Shoal cannot tell
//! which such workspace a coordinator's subagent works in, so each counts as
//! its coordinator's work. See `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::Records;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use shoal_ids::space_ids::SpaceId;

use super::tree::{self, TrackedAgents};
use crate::{
    model::{Space, SpaceKind},
    spaces::worktrees_mut,
    state::AgentState,
};

impl Spaces {
    pub async fn load_coordinator_states(
        &self,
        daemon: &dyn Daemon,
        spaces: &mut [Space],
    ) -> Result<()> {
        let agents = self.tracked_agents(daemon).await;
        let ids: Vec<SpaceId> = worktrees_mut(spaces).map(|(id, _)| id.clone()).collect();
        let records = self.records.clone();
        let states = self
            .store
            .run(move |db| {
                ids.iter()
                    .map(|id| coordinator_state(db, &*records, id, &agents))
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        for ((_, worktree), state) in worktrees_mut(spaces).zip(states) {
            worktree.coordinator_state = state;
        }
        Ok(())
    }

    /// Counts the agent in any turn state, since its subagents may work on
    /// past its turn. A coordinator above on another machine counts too,
    /// whatever that machine's link, as the hub lists it. While the hub
    /// cannot be asked, a space with a parent elsewhere counts as worked on,
    /// so cleanup never takes it unknowing.
    pub async fn coordinator_runs(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        let agents = self.tracked_agents(daemon).await;
        let owned = id.to_owned();
        let working = self
            .store
            .run(move |db| {
                Ok(match working_coordinator(db, &owned, &agents)? {
                    Working::Here(coordinator) => {
                        Some(tree::agent_runs(db, &coordinator, &agents)?)
                    }
                    Working::Above => None,
                    Working::Own => Some(false),
                })
            })
            .await?;
        match working {
            Some(runs) => Ok(runs),
            None => Ok(daemon.coordinator_elsewhere(id).await),
        }
    }
}

fn coordinator_state(
    db: &Connection,
    records: &dyn Records,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<AgentState>> {
    let Working::Here(coordinator) = working_coordinator(db, id, agents)? else {
        return Ok(None);
    };
    records.agent_state(db, &coordinator)
}

enum Working {
    Own,
    /// The nearest space above here with an agent.
    Here(SpaceId),
    /// None here does; one above on another machine may.
    Above,
}

/// The coordinator works while no agent of the space's own does: none
/// started in the workspace, or the chat's stopped.
fn working_coordinator(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<Working> {
    let kind: Option<(SpaceKind, bool)> = db
        .query_row(
            "SELECT kind,mode IS NULL FROM spaces WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let agentless = match kind {
        None => false,
        Some((SpaceKind::Workspace, modeless)) => modeless,
        Some((SpaceKind::Chat, _)) => crate::executions::execution_count(db, id, None)? == 0,
    };
    if !agentless {
        return Ok(Working::Own);
    }
    Ok(match tree::coordinator_of(db, id, agents)? {
        Some(coordinator) => Working::Here(coordinator),
        None => Working::Above,
    })
}

#[cfg(test)]
mod tests;
