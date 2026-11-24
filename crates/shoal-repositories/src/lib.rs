//! The daemon's registered repositories: registering and looking them up,
//! their configuration layers, keeping their checkouts' default branches
//! in sync, their seeds and shared caches, moving their
//! directories, removing them and the contribution clones Shoal removes on
//! its own. [`Repositories`] holds
//! what they keep in memory and offers their operations; what they need of
//! the rest of the daemon is the interface in [`daemon`].
use shoal_daemon_base::forge_access::ForgeAccess;
use std::sync::Arc;

use shoal_core::{config, paths::Paths};
use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, gates::Gates, notifications::Notifications,
};
use shoal_store::Store;

mod caches;
pub mod configuration;
pub mod contributions;
pub mod daemon;
pub mod moves;
pub mod records;
pub mod registration;
mod removal;
pub mod root;
pub mod seed;
pub mod spotlight;
#[cfg(test)]
mod stand_in;
pub mod sync;
#[cfg(feature = "test-support")]
mod test_support;

/// The repositories' service, which `Manager` holds.
pub struct Repositories {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    file_access: Arc<FileAccess>,
    forges: Arc<ForgeAccess>,
    paths: Paths,
    records: Arc<dyn daemon::Records>,
    contribution_holds: contributions::holds::ContributionHolds,
    /// The config files of worktrees the current format rejects, which the
    /// daemon ignores.
    rejected_configs: configuration::RejectedConfigs,
    seeds: seed::state::Seeds,
    /// What the syncs of each checkout's default branch found.
    syncs: sync::Syncs,
}

impl Repositories {
    #[allow(
        clippy::too_many_arguments,
        reason = "what the repositories share with the daemon's other subsystems"
    )]
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        file_access: Arc<FileAccess>,
        forges: Arc<ForgeAccess>,
        paths: Paths,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            file_access,
            forges,
            paths,
            records,
            contribution_holds: Default::default(),
            rejected_configs: Default::default(),
            seeds: Default::default(),
            syncs: Default::default(),
        }
    }

    /// The global config as it stands.
    fn config(&self) -> Arc<config::Config> {
        self.config.current()
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();
