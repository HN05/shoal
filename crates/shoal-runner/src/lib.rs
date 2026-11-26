//! The triggers that start agents, and the CI jobs this machine runs. See
//! `docs/design/runner.md`.
use shoal_daemon_base::forge_access::ForgeAccess;
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

pub struct Runner {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    forges: Arc<ForgeAccess>,
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
        forges: Arc<ForgeAccess>,
        records: Arc<dyn daemon::Records>,
        paths: Paths,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            forges,
            records,
            paths,
            triggers: Default::default(),
            jobs: Default::default(),
        }
    }

    fn config(&self) -> Arc<config::Config> {
        self.config.current()
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();
