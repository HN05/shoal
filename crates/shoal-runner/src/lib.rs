//! The daemon's triggers, and the CI jobs it runs for repositories' forges.
//! [`Runner`] holds what they keep in memory and offers their operations;
//! what they need of the rest of the daemon is the interface in [`daemon`].
use std::sync::Arc;

use shoal_core::config;
use shoal_daemon_base::{
    config_state::ConfigState, gates::Gates, log, notifications::Notifications,
};
use shoal_protocol::{paths::Paths, time};
use shoal_store::Store;

pub mod daemon;
pub mod jobs;
pub mod triggers;

#[cfg(test)]
mod stand_in;

/// The triggers' and the CI jobs' service, which `Manager` holds.
pub struct Runner {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    records: Arc<dyn daemon::Records>,
    paths: Paths,
    triggers: triggers::Triggers,
    jobs: jobs::Runners,
}

impl Runner {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        records: Arc<dyn daemon::Records>,
        paths: Paths,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            records,
            paths,
            triggers: Default::default(),
            jobs: Default::default(),
        }
    }

    /// The global config as it stands.
    fn config(&self) -> Arc<config::Config> {
        self.config.current()
    }
}
