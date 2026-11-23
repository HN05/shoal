//! What load protection stops: the newest one-shot run, which has no
//! session to lose and starts over once readings are healthy, otherwise
//! the first entry of the stop order whose stop frees what the pressure
//! presses. A CI job's stop always does, so agents stop only once no job
//! runs; a demo's or a heavy agent's does under memory pressure only where
//! its space holds memory worth freeing, and a demo's only once until
//! readings recover, since its wrapper runs it again at once. An agent
//! stops after every agent below it.
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
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
            log!("{}: stopped a one-shot agent run", pressure.reason());
            return true;
        }
        let turns = self.protection_turns().await;
        let order = self.stop_order(daemon, &turns).await;
        // A job or demo whose stop fails gives way to the next entry.
        for stop in (order.stops().iter()).filter(|stop| self.frees(daemon, stop, pressure)) {
            let stopped = match &stop.work {
                Work::Job => self.stop_job_for(daemon, stop, pressure).await,
                Work::Demo(execution) => {
                    self.restart_demo_for(daemon, stop, execution, pressure)
                        .await
                }
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
        let worth = || pressure != Pressure::Memory || self.holds_memory(daemon, &stop.space);
        match &stop.work {
            Work::Job => true,
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
        let stopped = daemon.stop_job_for_overload(&stop.space, reason);
        stopped.await.unwrap_or_else(|error| {
            log!("{reason}: CI job of {} not stopped: {error:#}", stop.space);
            false
        })
    }

    /// Restart the command of the demo `execution` in `stop`'s space for
    /// `pressure`, returning whether one ran.
    async fn restart_demo_for(
        &self,
        daemon: &dyn Daemon,
        stop: &Stop,
        execution: &ExecutionId,
        pressure: Pressure,
    ) -> bool {
        let reason = pressure.reason();
        self.overload_recovery.demo_restarted(execution);
        match daemon.restart_demo_command(execution).await {
            Ok(restarted) => {
                if restarted {
                    log!(
                        "{reason}: restarted the command of a demo in {}",
                        stop.space
                    );
                }
                restarted
            }
            Err(error) => {
                log!("{reason}: demo in {} not restarted: {error:#}", stop.space);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
