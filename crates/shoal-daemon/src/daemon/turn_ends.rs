//! Acting on spaces once their agents' turns end, through one loop that
//! looks whenever an agent's state changes and every few seconds besides:
//! a converted space's agent moves to its new place, and a recorded restart
//! restarts its agent.
use shoal_ids::space_ids::SpaceId;
use std::{sync::Arc, time::Duration};

use anyhow::Result;

use crate::{
    daemon::{log, workspace::Manager},
    model::RunningAgent,
    state::AgentState,
};

/// How often the spaces are looked at besides when an agent's state
/// changes, for what changes without it, such as a workspace whose setup
/// finished.
const RECHECK: Duration = Duration::from_secs(5);

/// Act on each space whose agent's turn ended, as long as the daemon runs.
pub(crate) async fn run(manager: Arc<Manager>) {
    let mut agents = manager.signals.watch_agents();
    loop {
        manager.act_on_turn_ends().await;
        tokio::select! {
            _ = agents.changed() => {}
            () = tokio::time::sleep(RECHECK) => {}
        }
    }
}

impl Manager {
    /// Do what waits for the end of an agent's turn, for each space whose
    /// agent's turn ended.
    async fn act_on_turn_ends(self: &Arc<Self>) {
        if let Err(error) = self.relocate_finished_turns().await {
            log!("conversions: {error:#}");
        }
        if let Err(error) = self.restart_finished_turns().await {
            log!("restarts: {error:#}");
        }
    }

    /// Whether the agent of the space `id` ended its turn: it reported
    /// itself idle, or no agent `runs` matches runs any longer.
    pub(crate) async fn turn_ended(
        &self,
        id: &SpaceId,
        runs: impl Fn(&RunningAgent) -> bool,
    ) -> Result<bool> {
        let space = id.to_owned();
        let state = self
            .store
            .read(move |db| crate::daemon::agent_state::current(db, &space))
            .await?;
        if state.is_some_and(|status| status.state == AgentState::Idle) {
            return Ok(true);
        }
        let running = self.running_agents().await?;
        Ok(!running.iter().any(runs))
    }
}
