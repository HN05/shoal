//! The local reviews the daemon runs: queueing them, running each once in
//! a detached checkout as a one-shot agent run, started in the background
//! without a session or terminal, opening a thread for each finding the
//! reviewer made and keeping the repository's false positives as those
//! threads are resolved, the review gates that wait for them, and the gate agent that decides approval requests. [`Reviews`] holds what they keep in memory and offers their
//! operations; what they need of the rest of the daemon is the interface in
//! [`daemon`].
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use shoal_core::{ai, config, env, fsutil, git, process, reviews, shoal_dirs};
use shoal_daemon_base::{gates::Gates, log};
use shoal_forge_types as forge;
#[cfg(test)]
use shoal_protocol::time;
use shoal_protocol::{agent, model, paths::Paths, validate};
use shoal_store::Store;
use tokio::sync::{Notify, watch};

#[cfg(test)]
use shoal_core::test_support;

mod answer;
mod checkouts;
mod code;
pub mod daemon;
mod delivery;
pub mod false_positives;
pub mod findings;
pub mod gate;
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
use daemon::{Records, Requests};
use reviews::ReviewId;

pub use run::run;

/// The daemon's reviews, with the daemon's services they use.
pub struct Reviews {
    store: Store,
    gates: Arc<Gates>,
    records: Arc<dyn Records>,
    requests: Arc<dyn Requests>,
    /// The daemon's paths: the home directory an agent's launch
    /// environment is laid over, and the state directory a gate run's
    /// `shoal` commands reach the daemon through.
    paths: Paths,
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
    /// The posts the reviewer ran for, which it runs for once, whatever
    /// came of it, until a newer post waits.
    answered: Mutex<std::collections::HashSet<daemon::AwaitedPost>>,
    gate: gate::Gatekeeper,
}

impl Reviews {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        records: Arc<dyn Records>,
        requests: Arc<dyn Requests>,
        paths: Paths,
    ) -> Self {
        Self {
            store,
            gates,
            records,
            requests,
            paths,
            queued: Notify::new(),
            cancels: Mutex::default(),
            checkouts: Mutex::default(),
            ended: watch::channel(0).0,
            runs: oneshot::Runs::default(),
            answered: Mutex::default(),
            gate: gate::Gatekeeper::default(),
        }
    }

    /// Stop what every one-shot run that has not ended cleanly left, as the
    /// daemon does after aborting them.
    pub async fn stop_runs(&self) {
        self.runs.stop_all().await;
    }

    /// The run directories of one-shot agent runs whose processes may still
    /// run, as their marker names them.
    pub fn unfinished_runs(&self) -> std::collections::HashSet<String> {
        self.runs.unfinished()
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
