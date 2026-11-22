//! The daemon's agents: the turn state their hooks report, the subagents
//! they run, retries of the turns API errors end, the alerts queued for
//! them, the user's engagement with them, idle wakes, sleep and wake
//! timers. [`Agents`] holds what they keep in memory and offers their
//! operations; what they need of the rest of the daemon is the interface in
//! [`daemon`].
use std::sync::Arc;

use shoal_core::{agents, ai, execution, process};
use shoal_daemon_base::{gates::Gates, log, signals::Signals};
use shoal_protocol::{agent, model, paths::Paths, protocol, state, time, validate};
use shoal_store::Store;

pub mod daemon;
pub mod engagement;
pub mod idle_wakes;
pub mod lost_reports;
pub mod messages;
#[cfg(test)]
mod stand_in;
pub mod subagents;
pub mod turn_retry;
pub mod turn_state;
pub mod turns;
pub mod wake_timers;

/// The agents' service, which `Manager` holds.
pub struct Agents {
    store: Store,
    gates: Arc<Gates>,
    signals: Arc<Signals>,
    paths: Paths,
    records: Arc<dyn daemon::Records>,
    /// When each space's agent state was last reported.
    report_times: lost_reports::ReportTimes,
    /// The time runs of agent messages settle by.
    settle_clock: messages::SettleClock,
}

impl Agents {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        signals: Arc<Signals>,
        paths: Paths,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            signals,
            paths,
            records,
            report_times: Default::default(),
            settle_clock: Default::default(),
        }
    }

    /// Forget the order and times of the state reports of the space `id`,
    /// which is gone.
    pub fn forget_reports(&self, id: &shoal_ids::space_ids::SpaceId) {
        self.report_times.forget(id);
    }

    /// The clock runs of agent messages settle by.
    pub fn settle_clock(&self) -> &messages::SettleClock {
        &self.settle_clock
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();
