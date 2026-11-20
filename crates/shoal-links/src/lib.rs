//! The issues, PRs and CI runs the daemon's spaces link. [`Links`] holds
//! what links keep in memory and offers their operations.
mod accounts;
pub mod daemon;
pub mod forges;
pub mod issue;
pub mod linked;
mod memories;
pub mod place;
pub mod pr;
mod raw;
pub mod reads;
pub mod records;
pub mod space;
#[cfg(test)]
mod stand_in;
pub mod target;
pub mod wait;

use std::sync::Arc;

use shoal_daemon_base::config_state::ConfigState;
use shoal_store::Store;

/// The links' service, which `Manager` holds.
pub struct Links {
    store: Store,
    config: Arc<ConfigState>,
    records: Arc<dyn daemon::Records>,
    memories: memories::ForgeMemories,
    waits: wait::ActiveWaits,
}

impl Links {
    pub fn new(store: Store, config: Arc<ConfigState>, records: Arc<dyn daemon::Records>) -> Self {
        Self {
            store,
            config,
            records,
            memories: Default::default(),
            waits: Default::default(),
        }
    }

    /// The running wait of each space.
    pub fn waits(&self) -> &wait::ActiveWaits {
        &self.waits
    }
}
