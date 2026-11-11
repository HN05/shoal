//! The global config the daemon runs under, and readings that hold only
//! under the config they were taken with.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::sync::{Mutex, MutexGuard, watch};

use crate::config::Config;

pub(crate) struct ConfigState {
    /// The global file as last loaded; a reload replaces it for later reads
    /// and wakes subscribers.
    config: watch::Sender<Arc<Config>>,
    reload: Mutex<()>,
    /// Counts published configurations, so a reading taken under an earlier
    /// one cannot pass for a current one.
    generation: AtomicU64,
    /// One more than the generation under which the disk monitor's latest
    /// reading found the cleanup threshold free on every filesystem, so
    /// restored agents can write; zero for none.
    disk_recovered_in: AtomicU64,
}

impl ConfigState {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            config: watch::channel(Arc::new(config)).0,
            reload: Mutex::new(()),
            generation: AtomicU64::new(0),
            disk_recovered_in: AtomicU64::new(0),
        }
    }

    /// The config as last loaded. Take one snapshot per operation so a
    /// concurrent reload cannot mix two versions.
    pub(crate) fn current(&self) -> Arc<Config> {
        self.config.borrow().clone()
    }

    /// Observe reloads, for waits that a policy change must end.
    pub(crate) fn watch(&self) -> watch::Receiver<Arc<Config>> {
        self.config.subscribe()
    }

    /// Make `config` the one later operations read.
    pub(crate) fn publish(&self, config: Config) {
        self.config.send_replace(Arc::new(config));
        // After the swap: a reader that sees the new generation sees this config.
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Order reloads so the last file read is the one published.
    pub(crate) async fn lock_reload(&self) -> MutexGuard<'_, ()> {
        self.reload.lock().await
    }

    /// The generation of the current config. Read it before the config a
    /// reading uses, so a reload in between makes the reading stale.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Record the generation under which a reading found the cleanup
    /// threshold free everywhere, or that the latest one did not.
    pub(crate) fn publish_disk_reading(&self, recovered_in: Option<u64>) {
        let encoded = recovered_in.map_or(0, |generation| generation + 1);
        self.disk_recovered_in.store(encoded, Ordering::SeqCst);
    }

    /// Whether the latest reading found the cleanup threshold free everywhere
    /// under the current config; a reload waits for a new reading.
    pub(crate) fn disk_space_recovered(&self) -> bool {
        self.disk_recovered_in.load(Ordering::SeqCst) == self.generation() + 1
    }
}
