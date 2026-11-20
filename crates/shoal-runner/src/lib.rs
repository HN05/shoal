//! The daemon's triggers. [`Runner`] holds what they keep in memory and
//! offers their operations; what they need of the rest of the daemon is the
//! interface in [`daemon`].
use std::sync::Arc;

use shoal_core::config;
use shoal_daemon_base::{config_state::ConfigState, log, notifications::Notifications};
use shoal_protocol::time;
use shoal_store::Store;

pub mod daemon;
pub mod triggers;

#[cfg(test)]
mod stand_in;

/// The triggers' service, which `Manager` holds.
pub struct Runner {
    store: Store,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    triggers: triggers::Triggers,
}

impl Runner {
    pub fn new(store: Store, config: Arc<ConfigState>, notifications: Arc<Notifications>) -> Self {
        Self {
            store,
            config,
            notifications,
            triggers: Default::default(),
        }
    }

    /// The global config as it stands.
    fn config(&self) -> Arc<config::Config> {
        self.config.current()
    }
}
