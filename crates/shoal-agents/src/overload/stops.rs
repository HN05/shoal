//! What load protection stops next. `docs/design/protection.md` gives the order
//! and why.
use shoal_ids::{execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_schedule::stopping::{Stop, Work};

use super::Pressure;
use crate::Agents;
use crate::daemon::Daemon;
use crate::log;
use crate::queue::OneShotStop;

/// Memory below which stopping a space's work frees too little to be
/// worth its loss.
const NEGLIGIBLE_MEMORY: u64 = 64 << 20;

impl Agents {
    /// Stop work under `pressure`, returning whether anything stopped.
    pub async fn stop_for_overload(&self, daemon: &dyn Daemon, pressure: Pressure) -> bool {
        if self.agent_queue.stop_oneshots(OneShotStop::Newest) > 0 {
            log!("{}: stopped a review", pressure.reason());
            return true;
        }
        let turns = self.protection_turns().await;
        let order = self.stop_order(daemon, &turns).await;
        // A job or demo whose stop fails gives way to the next entry.
        for stop in (order.stops().iter()).filter(|stop| self.frees(daemon, stop, pressure)) {
            let stopped = match &stop.work {
                Work::Job => self.stop_job_for(daemon, stop, pressure).await,
                Work::Run(id) => self.stop_run_for(id, pressure),
                Work::Demo(execution) => self.restart_demo_for(daemon, execution, pressure).await,
                // An asleep agent's running lease goes before any agent,
                // which it picks among itself, and whose stop starts a new
                // recovery epoch.
                Work::Agent { .. } => {
                    if self.stop_asleep_lease_for(daemon, pressure).await {
                        true
                    } else {
                        return self.stop_agent_for_overload(daemon, pressure, turns).await;
                    }
                }
            };
            if stopped {
                self.overload_recovery.reset();
                return true;
            }
        }
        // With no agent to stop, an asleep agent's lease may still free some.
        let stopped = self.stop_asleep_lease_for(daemon, pressure).await;
        if stopped {
            self.overload_recovery.reset();
        }
        stopped
    }

    /// Stop one running lease of an asleep agent for `pressure`, as only
    /// memory pressure stops them, returning whether one stopped.
    async fn stop_asleep_lease_for(&self, daemon: &dyn Daemon, pressure: Pressure) -> bool {
        let stopped = pressure == Pressure::Memory && daemon.stop_a_sleepers_running_lease().await;
        if stopped {
            log!("{}: stopped an asleep agent's lease", pressure.reason());
        }
        stopped
    }

    /// Whether stopping `stop` for its own sake frees what `pressure`
    /// presses.
    pub fn frees(&self, daemon: &dyn Daemon, stop: &Stop, pressure: Pressure) -> bool {
        let worth = || {
            let space = stop.space.as_ref();
            pressure != Pressure::Memory
                || space.is_none_or(|space| self.holds_memory(daemon, space))
        };
        match &stop.work {
            Work::Job => true,
            Work::Run(id) => {
                let using = self.agent_queue.measured_using(id);
                pressure != Pressure::Memory
                    || using.is_some_and(|using| using >= NEGLIGIBLE_MEMORY)
            }
            Work::Demo(execution) => !self.overload_recovery.restarted_demo(execution) && worth(),
            Work::Agent { light, .. } => !light && worth(),
        }
    }

    /// Whether the processes of `space` hold memory worth freeing, as its
    /// latest usage sample shows; one without a sample may.
    fn holds_memory(&self, daemon: &dyn Daemon, space: &SpaceId) -> bool {
        daemon
            .workspace_usage(space)
            .is_none_or(|usage| usage.memory_bytes >= NEGLIGIBLE_MEMORY)
    }

    /// End the CI job `stop` names for `pressure`, to rerun once readings
    /// recover, returning whether it did.
    async fn stop_job_for(&self, daemon: &dyn Daemon, stop: &Stop, pressure: Pressure) -> bool {
        let reason = pressure.reason();
        let Some(space) = &stop.space else {
            return false;
        };
        let stopped = daemon.stop_job_for_overload(space, reason);
        stopped.await.unwrap_or_else(|error| {
            log!("{reason}: CI job of {space} not stopped: {error:#}");
            false
        })
    }

    /// Stop the command or seed build that waited under `id` for
    /// `pressure`, to run again once it fits, returning whether it did.
    fn stop_run_for(&self, id: &QueueEntryId, pressure: Pressure) -> bool {
        let stopped = self.agent_queue.stop_measured(id, pressure.reason());
        if stopped {
            log!("{}: stopped {id}", pressure.reason());
        }
        stopped
    }

    /// Restart the command of the demo `execution` for `pressure`,
    /// returning whether one ran.
    async fn restart_demo_for(
        &self,
        daemon: &dyn Daemon,
        execution: &ExecutionId,
        pressure: Pressure,
    ) -> bool {
        let reason = pressure.reason();
        self.overload_recovery.demo_restarted(execution);
        match daemon.restart_demo_command(execution).await {
            Ok(restarted) => {
                if restarted {
                    log!("{reason}: restarted the command of demo {execution}");
                }
                restarted
            }
            Err(error) => {
                log!("{reason}: demo {execution} not restarted: {error:#}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
