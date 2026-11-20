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
mod reads;
pub mod space;
#[cfg(test)]
mod stand_in;
pub mod target;
pub mod wait;

use std::sync::Arc;

use shoal_daemon_base::config_state::ConfigState;

/// The links' service, which `Manager` holds.
pub struct Links {
    config: Arc<ConfigState>,
    memories: memories::ForgeMemories,
    waits: wait::ActiveWaits,
}

impl Links {
    pub fn new(config: Arc<ConfigState>) -> Self {
        Self {
            config,
            memories: Default::default(),
            waits: Default::default(),
        }
    }

    /// The running wait of each space.
    pub fn waits(&self) -> &wait::ActiveWaits {
        &self.waits
    }
}
