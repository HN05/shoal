//! The issues, PRs and CI runs the daemon's spaces link. [`Links`] holds
//! what links keep in memory and offers their operations.
mod accounts;
pub mod actions;
pub mod branches;
mod checks;
mod ci;
pub mod daemon;
pub mod forges;
pub mod issue;
pub mod linked;
mod linking;
mod memories;
mod merge_path;
pub mod place;
pub mod pr;
mod raw;
pub mod reads;
pub mod records;
mod relink;
mod run_approval;
pub mod runs;
pub mod space;
#[cfg(test)]
mod stand_in;
pub mod target;
mod view;
pub mod wait;

use std::sync::Arc;

use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, gates::Gates, notifications::Notifications,
};
use shoal_store::Store;

/// The links' service, which `Manager` holds.
pub struct Links {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    file_access: Arc<FileAccess>,
    records: Arc<dyn daemon::Records>,
    memories: memories::ForgeMemories,
    waits: wait::ActiveWaits,
    watch_starts: wait::starts::WatchStarts,
    agent_settles: issue::agent_settles::AgentSettles,
}

impl Links {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        file_access: Arc<FileAccess>,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            file_access,
            records,
            memories: Default::default(),
            waits: Default::default(),
            watch_starts: Default::default(),
            agent_settles: Default::default(),
        }
    }

    /// The running wait of each space.
    pub fn waits(&self) -> &wait::ActiveWaits {
        &self.waits
    }
}
