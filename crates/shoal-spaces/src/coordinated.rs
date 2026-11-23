//! Spaces whose coordinator does their work itself: workspaces in which no
//! agent of their own has started, as a split child before `shoal start`
//! gives it one, which the workspace's unrecorded mode tells, and chats
//! whose agent stopped. The coordinator's subagents work there from inside
//! its agent, reporting under its scope from its directory, so Shoal cannot
//! tell which such workspace a subagent works in; each counts as its
//! coordinator's work, and a stopped chat's work is left to it.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::Records;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use shoal_ids::space_ids::SpaceId;

use super::tree::{self, TrackedAgents};
use crate::{
    model::{Space, SpaceKind},
    state::AgentState,
    {spaces::worktrees_mut, store},
};

impl Spaces {
    /// Fill in, for each workspace among `spaces` whose coordinator does
    /// its work, the turn state that coordinator's agent reports.
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

    /// Whether the agent of the coordinator that does the work of the space
    /// `id` runs, in any turn state, since its subagents may work on past
    /// its turn: one here, or above it on another machine with an agent,
    /// whatever that machine's link, as the hub lists it; while the hub
    /// cannot be asked, a space with a parent elsewhere counts as worked
    /// on, so cleanup never takes it unknowing.
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

/// The turn state the agent of the coordinator that does the work of
/// workspace `id` reports.
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

/// Who does the work of a space.
enum Working {
    /// An agent of its own has started there.
    Own,
    /// The coordinator here, the nearest space above with an agent.
    Here(SpaceId),
    /// None here does; one above on another machine may.
    Above,
}

/// Who does the work of the space `id`: its coordinator while no agent of
/// its own works there, as none has started in the workspace, or the chat's
/// has stopped.
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
        Some((SpaceKind::Chat, _)) => store::execution_count(db, id, None)? == 0,
    };
    if !agentless {
        return Ok(Working::Own);
    }
    Ok(match tree::coordinator_of(db, id, agents)? {
        Some(coordinator) => Working::Here(coordinator),
        None => Working::Above,
    })
}
