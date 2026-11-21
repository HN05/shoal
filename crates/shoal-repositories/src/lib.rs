//! The daemon's registered repositories: registering and looking them up,
//! their configuration layers, moving their directories, removing them and
//! the contribution clones Shoal removes on its own. [`Repositories`] holds
//! what they keep in memory and offers their operations; what they need of
//! the rest of the daemon is the interface in [`daemon`].
use std::sync::Arc;

use shoal_core::{config, paths::Paths};
use shoal_daemon_base::{config_state::ConfigState, gates::Gates};
use shoal_store::Store;

pub mod contributions;
pub mod daemon;
mod records;
pub mod registration;
pub mod root;
pub mod spotlight;
#[cfg(test)]
mod stand_in;

/// The repositories' service, which `Manager` holds.
pub struct Repositories {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    paths: Paths,
    records: Arc<dyn daemon::Records>,
    contribution_holds: contributions::holds::ContributionHolds,
}

impl Repositories {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        paths: Paths,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            paths,
            records,
            contribution_holds: Default::default(),
        }
    }

    /// The global config as it stands.
    fn config(&self) -> Arc<config::Config> {
        self.config.current()
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();
