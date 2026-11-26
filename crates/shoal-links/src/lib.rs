//! The issues, PRs and CI runs spaces link, the polls that read them and
//! the watches that report them.
pub mod actions;
pub mod branches;
mod checks;
mod ci;
pub mod daemon;
pub mod forges;
pub mod issue;
pub mod linked;
mod linking;
mod merge_path;
mod merge_review;
pub mod merge_warning;
pub mod place;
pub mod pr;
pub mod proposals;
pub mod pushes;
mod raw;
pub mod reads;
pub mod records;
mod relink;
mod revert;
mod run_approval;
pub mod runs;
pub mod space;
pub mod stack;
#[cfg(test)]
mod stand_in;
pub mod target;
mod verdict;
mod view;
pub mod wait;

use shoal_daemon_base::operations::Operations;
use std::sync::Arc;

use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, forge_access::ForgeAccess, gates::Gates,
    notifications::Notifications,
};
use shoal_store::Store;

pub struct Links {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    file_access: Arc<FileAccess>,
    forges: Arc<ForgeAccess>,
    operations: Arc<Operations>,
    records: Arc<dyn daemon::Records>,
    waits: wait::ActiveWaits,
    watch_starts: wait::starts::WatchStarts,
    agent_settles: issue::agent_settles::AgentSettles,
}

impl Links {
    #[expect(
        clippy::too_many_arguments,
        reason = "what the links share with the daemon's other subsystems"
    )]
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        file_access: Arc<FileAccess>,
        forges: Arc<ForgeAccess>,
        operations: Arc<Operations>,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            file_access,
            forges,
            operations,
            records,
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

#[cfg(test)]
shoal_core::prepare_test_process!();
