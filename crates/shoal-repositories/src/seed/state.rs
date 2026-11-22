//! What the daemon keeps in memory about each repository's seed builds,
//! and the guards that keep them out of a repository.
use shoal_ids::repository_ids::RepositoryId;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedMutexGuard, OwnedRwLockWriteGuard, RwLock, watch};

use crate::Repositories;

/// The build of a seed: the commit and the settings it was built with.
pub type Build = (shoal_refs::CommitId, String);

#[derive(Default)]
pub struct Seeds {
    /// The build that last failed, per repository.
    failures: Mutex<HashMap<RepositoryId, Build>>,
    /// Running seed builds, which repository removal cancels and waits for.
    builds: Mutex<HashMap<RepositoryId, Arc<SeedSlot>>>,
    /// Woken when a snapshot needs a spare.
    spares: tokio::sync::Notify,
}

impl Seeds {
    /// The repository's build slot.
    pub fn slot(&self, repository_id: &RepositoryId) -> Arc<SeedSlot> {
        self.builds
            .lock()
            .expect("seed builds lock")
            .entry(repository_id.to_owned())
            .or_default()
            .clone()
    }

    /// Whether `build` is the one that last failed; any other build makes
    /// the failed one worth retrying when it comes back.
    pub(crate) fn repeats_failure(&self, repository_id: &RepositoryId, build: &Build) -> bool {
        let mut failures = self.failures.lock().expect("seed failures lock");
        if failures.get(repository_id) == Some(build) {
            return true;
        }
        failures.remove(repository_id);
        false
    }

    /// Have the spare task refill spares: a workspace took one, or a new
    /// snapshot has none.
    pub(crate) fn want_spare(&self) {
        self.spares.notify_one();
    }

    /// Wait until [`Self::want_spare`] asks for a refill, or return at once
    /// when it asked since the last wait.
    pub async fn spare_wanted(&self) {
        self.spares.notified().await;
    }

    /// Remember the build that failed, or forget an earlier failure.
    pub(crate) fn record_result(&self, repository_id: &RepositoryId, failed: Option<Build>) {
        let mut failures = self.failures.lock().expect("seed failures lock");
        match failed {
            Some(build) => failures.insert(repository_id.to_owned(), build),
            None => failures.remove(repository_id),
        };
    }
}

/// One repository's seed build: held while a build runs, and set to cancel it.
pub struct SeedSlot {
    gate: Arc<tokio::sync::Mutex<()>>,
    cancel: watch::Sender<bool>,
    /// Read while a clone of the snapshot's outputs runs, and written while
    /// the snapshot is swapped, which a build does only once it has built,
    /// or deleted.
    snapshot: Arc<RwLock<()>>,
    /// Held while a new worktree takes the spare, which has one taker.
    spare: tokio::sync::Mutex<()>,
}

impl Default for SeedSlot {
    fn default() -> Self {
        Self {
            gate: Default::default(),
            cancel: watch::channel(false).0,
            snapshot: Default::default(),
            spare: Default::default(),
        }
    }
}

impl SeedSlot {
    /// Held while a build runs.
    pub(crate) fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }

    /// Whether the running build should stop.
    pub(crate) fn cancelled(&self) -> watch::Receiver<bool> {
        self.cancel.subscribe()
    }

    /// Read while a clone of the snapshot's outputs runs, and written while
    /// it is swapped or deleted.
    pub fn snapshot(&self) -> &Arc<RwLock<()>> {
        &self.snapshot
    }

    /// Held while a new worktree takes the spare.
    pub(crate) fn spare(&self) -> &tokio::sync::Mutex<()> {
        &self.spare
    }
}

/// Keeps seed builds out of a repository; dropping it allows them again.
pub struct SeedPause {
    _guard: OwnedMutexGuard<()>,
    slot: Arc<SeedSlot>,
}

impl SeedPause {
    /// Keep spare refills and clones of the snapshot out too, once those
    /// under way end. Only clones hold the snapshot while builds are
    /// paused, so the Git gate may be held already.
    pub async fn stop(self) -> SeedStop {
        let snapshot = self.slot.snapshot.clone().write_owned().await;
        SeedStop {
            _pause: self,
            _snapshot: snapshot,
        }
    }
}

impl Drop for SeedPause {
    fn drop(&mut self) {
        self.slot.cancel.send_replace(false);
    }
}

/// Keeps seed builds, spare refills and clones of the snapshot out of a
/// repository; dropping it allows them again.
pub struct SeedStop {
    _pause: SeedPause,
    _snapshot: OwnedRwLockWriteGuard<()>,
}

impl Repositories {
    /// What the daemon keeps of the repositories' seed builds.
    pub fn seeds(&self) -> &Seeds {
        &self.seeds
    }

    /// [`Self::pause_seed_build`], then [`SeedPause::stop`].
    pub async fn stop_seed_build(&self, repository_id: &RepositoryId) -> SeedStop {
        self.pause_seed_build(repository_id).await.stop().await
    }

    /// Stop the repository's seed build, if one runs, and keep new ones out
    /// until the returned guard drops, while new worktrees still clone the
    /// snapshot. Take it before the Git gate, which a build takes while
    /// holding its own.
    pub async fn pause_seed_build(&self, repository_id: &RepositoryId) -> SeedPause {
        let slot = self.seeds.slot(repository_id);
        slot.cancel.send_replace(true);
        let guard = slot.gate.clone().lock_owned().await;
        SeedPause {
            _guard: guard,
            slot,
        }
    }

    /// Keep the repository's seed builds from starting until the returned
    /// guard drops, or `None` while one runs.
    pub fn pause_idle_seed(&self, repository_id: &RepositoryId) -> Option<OwnedMutexGuard<()>> {
        self.seeds
            .slot(repository_id)
            .gate
            .clone()
            .try_lock_owned()
            .ok()
    }
}
