//! What the daemon keeps in memory about each repository's seed builds.
use crate::client::repository_ids::RepositoryId;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::SeedSlot;

/// The build of a seed: the commit and the settings it was built with.
pub(super) type Build = (String, String);

#[derive(Default)]
pub(crate) struct Seeds {
    /// The build that last failed, per repository.
    failures: Mutex<HashMap<RepositoryId, Build>>,
    /// Running seed builds, which repository removal cancels and waits for.
    builds: Mutex<HashMap<RepositoryId, Arc<SeedSlot>>>,
    /// Woken when a snapshot needs a spare.
    spares: tokio::sync::Notify,
}

impl Seeds {
    /// The repository's build slot.
    pub(super) fn slot(&self, repository_id: &RepositoryId) -> Arc<SeedSlot> {
        self.builds
            .lock()
            .expect("seed builds lock")
            .entry(repository_id.to_owned())
            .or_default()
            .clone()
    }

    /// Whether `build` is the one that last failed; any other build makes
    /// the failed one worth retrying when it comes back.
    pub(super) fn repeats_failure(&self, repository_id: &RepositoryId, build: &Build) -> bool {
        let mut failures = self.failures.lock().expect("seed failures lock");
        if failures.get(repository_id) == Some(build) {
            return true;
        }
        failures.remove(repository_id);
        false
    }

    /// Have the spare task refill spares: a workspace took one, or a new
    /// snapshot has none.
    pub(super) fn want_spare(&self) {
        self.spares.notify_one();
    }

    /// Wait until [`Self::want_spare`] asks for a refill, or return at once
    /// when it asked since the last wait.
    pub(super) async fn spare_wanted(&self) {
        self.spares.notified().await;
    }

    /// Remember the build that failed, or forget an earlier failure.
    pub(super) fn record_result(&self, repository_id: &RepositoryId, failed: Option<Build>) {
        let mut failures = self.failures.lock().expect("seed failures lock");
        match failed {
            Some(build) => failures.insert(repository_id.to_owned(), build),
            None => failures.remove(repository_id),
        };
    }
}
