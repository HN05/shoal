//! Ranking the queue: the policy `machine.schedule` names, over the
//! snapshot the daemon builds, orders the waiting entries, spaces in the
//! order their agents would start, and running work in the order it stops.
use anyhow::Result;

use crate::Agents;
use crate::config::machine::Schedule;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use shoal_schedule::{
    leads::{LongestChain, Nothing, WarmCache},
    priority::Priority,
    ranking::Ranking,
    scheduler::{Ranked, Scheduler},
    snapshot::Snapshot,
    stopping::{Running, StopOrder},
};

impl Agents {
    /// The waiting entries, one-shot runs among them, in the order they
    /// start, each with its rank.
    pub async fn ranked_queue(&self, daemon: &dyn Daemon) -> Result<Vec<Ranked>> {
        let snapshot = daemon.ranking_snapshot().await?;
        Ok(scheduler(self.config.current().machine.schedule).rank(&snapshot))
    }

    /// The spaces `ids` in the order their agents would start, first
    /// first, each with its priority, as the queue ranks its entries.
    pub async fn ranked_spaces(
        &self,
        daemon: &dyn Daemon,
        ids: &[SpaceId],
    ) -> Result<Vec<(SpaceId, Priority)>> {
        let snapshot = daemon.ranking_snapshot().await?;
        Ok(scheduler(self.config.current().machine.schedule).spaces_in_order(&snapshot, ids))
    }

    /// The `running` work in the order the policy stops it; without the
    /// queue's rank, its agents' spaces rank alike.
    pub async fn order_stops(&self, daemon: &dyn Daemon, running: Running) -> StopOrder {
        let mut snapshot = daemon.ranking_snapshot().await.unwrap_or_else(|error| {
            crate::log!("queue rank unavailable to the stop order: {error:#}");
            Snapshot::default()
        });
        snapshot.running = running;
        scheduler(self.config.current().machine.schedule).stop_order(&snapshot)
    }
}

/// The policy that orders the queue under `schedule`.
fn scheduler(schedule: Schedule) -> Box<dyn Scheduler> {
    match schedule {
        Schedule::Balanced => Box::new(Ranking::new(Nothing)),
        Schedule::Speed => Box::new(Ranking::new(LongestChain)),
        Schedule::Cache => Box::new(Ranking::new(WarmCache)),
    }
}
