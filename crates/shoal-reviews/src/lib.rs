//! The local reviews the daemon runs: queueing them, running each once in
//! a detached checkout as a one-shot agent run, started in the background
//! without a session or terminal, recording what the reviewer found and how
//! the workspace's agent answered it, and the review gates that wait for
//! them. [`Reviews`] holds what they keep in memory and offers their
//! operations; what they need of the rest of the daemon is the interface in
//! [`daemon`].
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use shoal_core::{ai, config, env, fsutil, git, process, reviews, shoal_dirs};
use shoal_daemon_base::{gates::Gates, log};
use shoal_forge_types as forge;
#[cfg(test)]
use shoal_protocol::time;
use shoal_protocol::{agent, model, validate};
use shoal_store::Store;
use tokio::sync::{Notify, watch};

#[cfg(test)]
use shoal_core::test_support;

mod checkouts;
pub mod daemon;
mod delivery;
pub mod false_positives;
mod findings;
pub mod history;
pub mod oneshot;
mod post;
mod queue;
pub mod required;
pub mod reviewer;
mod run;
#[cfg(test)]
mod stand_in;
pub mod store;
mod view;

use checkouts::HeldCheckout;
use daemon::Records;
use reviews::ReviewId;

pub use run::run;

/// The daemon's reviews, with the daemon's services they use.
pub struct Reviews {
    store: Store,
    gates: Arc<Gates>,
    records: Arc<dyn Records>,
    /// The home directory an agent's launch environment is laid over.
    home: PathBuf,
    /// Wakes the scheduler when a review is queued.
    queued: Notify,
    /// Stops each review that has started to run.
    cancels: Mutex<HashMap<ReviewId, Arc<watch::Sender<bool>>>>,
    /// The repository and run of each review whose checkout, a worktree of
    /// the repository, may exist, which outlives the review's records when
    /// its workspace goes.
    checkouts: Mutex<HashMap<ReviewId, HeldCheckout>>,
    /// Counts the runs that ended, for waits on one.
    ended: watch::Sender<u64>,
    /// One-shot agent runs whose processes may still be running.
    runs: oneshot::Runs,
}

impl Reviews {
    pub fn new(store: Store, gates: Arc<Gates>, records: Arc<dyn Records>, home: PathBuf) -> Self {
        Self {
            store,
            gates,
            records,
            home,
            queued: Notify::new(),
            cancels: Mutex::default(),
            checkouts: Mutex::default(),
            ended: watch::channel(0).0,
            runs: oneshot::Runs::default(),
        }
    }

    /// The one-shot runs whose processes may still be running, which the
    /// gate's runs share.
    pub fn runs(&self) -> &oneshot::Runs {
        &self.runs
    }

    /// Wait until a review's run ends, after subscribing.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ends(&self) -> watch::Receiver<u64> {
        self.ended.subscribe()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
shoal_core::prepare_test_process!();
