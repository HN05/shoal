//! Acting on spaces once their agents' turns end, through one loop that
//! looks whenever an agent's state changes and every few seconds besides,
//! for whatever Shoal recorded to happen to an agent or its space only
//! between turns.
use std::{sync::Arc, time::Duration};

use crate::daemon::{log, manager::Manager};

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
        if let Err(error) = self.spaces.relocate_finished_turns(&**self).await {
            log!("conversions: {error:#}");
        }
        let daemon: Arc<dyn shoal_agents::daemon::Daemon> = self.clone();
        if let Err(error) = self.agents.restart_finished_turns(&daemon).await {
            log!("restarts: {error:#}");
        }
        if let Err(error) = self.spaces.start_moves_after_turns(&**self).await {
            log!("moves after turns: {error:#}");
        }
    }
}
