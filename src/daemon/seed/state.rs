//! What the daemon keeps in memory about each repository's seed builds.
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
    failures: Mutex<HashMap<String, Build>>,
    /// Running seed builds, which repository removal cancels and waits for.
    builds: Mutex<HashMap<String, Arc<SeedSlot>>>,
}

impl Seeds {
    /// The repository's build slot.
    pub(super) fn slot(&self, repository_id: &str) -> Arc<SeedSlot> {
        self.builds
            .lock()
            .expect("seed builds lock")
            .entry(repository_id.to_owned())
            .or_default()
            .clone()
    }

    /// Whether `build` is the one that last failed; any other build makes
    /// the failed one worth retrying when it comes back.
    pub(super) fn repeats_failure(&self, repository_id: &str, build: &Build) -> bool {
        let mut failures = self.failures.lock().expect("seed failures lock");
        if failures.get(repository_id) == Some(build) {
            return true;
        }
        failures.remove(repository_id);
        false
    }

    /// Remember the build that failed, or forget an earlier failure.
    pub(super) fn record_result(&self, repository_id: &str, failed: Option<Build>) {
        let mut failures = self.failures.lock().expect("seed failures lock");
        match failed {
            Some(build) => failures.insert(repository_id.to_owned(), build),
            None => failures.remove(repository_id),
        };
    }
}
