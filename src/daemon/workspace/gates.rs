//! The daemon's gates: each orders one kind of change against the others
//! that would conflict with it, and is taken through the operation named
//! for it.
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{
    Mutex, MutexGuard, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
    RwLockReadGuard, RwLockWriteGuard,
};

use keyed::Keyed;

mod keyed;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct Gates {
    background_operations: Arc<RwLock<()>>,
    registry: Mutex<()>,
    checkpoints: Mutex<()>,
    simulators: Mutex<()>,
    proposal_files: Mutex<()>,
    announcements: Mutex<()>,
    git: Keyed<Mutex<()>>,
    forge_hosts: Keyed<Mutex<()>>,
    resources: Keyed<RwLock<()>>,
    prs: Keyed<Mutex<()>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum GuardMode {
    Shared,
    Exclusive,
}

/// The held PR gates of one or more workspaces.
pub(crate) struct PrGuard {
    _guards: Vec<OwnedMutexGuard<()>>,
}

pub(crate) enum ResourceGuard {
    Operation { _guard: OwnedRwLockReadGuard<()> },
    Hook { _guard: OwnedRwLockWriteGuard<()> },
}

impl Gates {
    /// Cover a background operation, so a daemon update can quiesce
    /// operations without cancelling hooks or mutations already in progress.
    pub(crate) async fn operation(&self) -> RwLockReadGuard<'_, ()> {
        self.background_operations.read().await
    }

    /// [`Self::operation`] for a guard that outlives the borrow.
    pub(crate) async fn owned_operation(&self) -> OwnedRwLockReadGuard<()> {
        self.background_operations.clone().read_owned().await
    }

    /// [`Self::operation`] unless an update is quiescing operations.
    pub(crate) fn try_operation(&self) -> Option<RwLockReadGuard<'_, ()>> {
        self.background_operations.try_read().ok()
    }

    /// [`Self::owned_operation`] unless an update is quiescing operations.
    pub(crate) fn try_owned_operation(&self) -> Option<OwnedRwLockReadGuard<()>> {
        self.background_operations.clone().try_read_owned().ok()
    }

    /// Hold off every background operation once those in progress finish.
    #[cfg(test)]
    pub(crate) async fn quiesce(&self) -> RwLockWriteGuard<'_, ()> {
        self.background_operations.write().await
    }

    /// [`Self::quiesce`] when no background operation is in progress.
    pub(crate) fn try_quiesce(&self) -> Option<RwLockWriteGuard<'_, ()>> {
        self.background_operations.try_write().ok()
    }

    /// Serialize repository registration, removal, and local config changes.
    pub(crate) async fn lock_registry(&self) -> MutexGuard<'_, ()> {
        self.registry.lock().await
    }

    /// Order checkpoint records and refs against expiry, so a ref is never
    /// deleted between its record and its write. Hold it only for those
    /// steps, since removal takes checkpoints under the repository's Git gate.
    pub(crate) async fn lock_checkpoints(&self) -> MutexGuard<'_, ()> {
        self.checkpoints.lock().await
    }

    /// Serialize every simctl transition.
    pub(crate) async fn lock_simulators(&self) -> MutexGuard<'_, ()> {
        self.simulators.lock().await
    }

    /// [`Self::lock_simulators`] when no transition is in progress.
    pub(crate) fn try_lock_simulators(&self) -> Option<MutexGuard<'_, ()>> {
        self.simulators.try_lock().ok()
    }

    /// Order copying a proposal's files and storing it against pruning, so
    /// pruning never deletes the copies of a proposal not yet stored.
    pub(crate) async fn lock_proposal_files(&self) -> MutexGuard<'_, ()> {
        self.proposal_files.lock().await
    }

    /// Order announcing a question or approval request against withdrawing
    /// it, so a withdrawn one is never announced afterwards.
    pub(crate) async fn lock_announcements(&self) -> MutexGuard<'_, ()> {
        self.announcements.lock().await
    }

    /// Hold the repository's gate for ref updates and worktree creation and
    /// removal, without checking that the repository is available. Callers
    /// must not already hold it.
    pub(crate) async fn lock_git(&self, repository_id: &str) -> OwnedMutexGuard<()> {
        self.git.gate(repository_id).lock_owned().await
    }

    /// [`Self::lock_git`] when nothing holds the gate.
    pub(crate) fn try_lock_git(&self, repository_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.git.gate(repository_id).try_lock_owned().ok()
    }

    /// The Git gate of `repository_id`, for tests that count who waits on it.
    #[cfg(test)]
    pub(crate) fn git_gate(&self, repository_id: &str) -> Arc<Mutex<()>> {
        self.git.gate(repository_id)
    }

    /// Hold `host`'s gate, so concurrent first lookups ask a forge host once.
    pub(crate) async fn lock_forge_host(&self, host: &str) -> OwnedMutexGuard<()> {
        self.forge_hosts.gate(host).lock_owned().await
    }

    /// The gate of `host`, for tests that count who waits on it.
    #[cfg(test)]
    pub(crate) fn forge_host_gate(&self, host: &str) -> Arc<Mutex<()>> {
        self.forge_hosts.gate(host)
    }

    /// Exclude lifecycle transitions and overlapping permit hooks in the
    /// workspace `id`: a hook takes it exclusively, an operation shared.
    pub(crate) fn resource_guard(&self, id: &str, mode: GuardMode) -> Result<ResourceGuard> {
        let gate = self.resources.gate(id);
        let guard = match mode {
            GuardMode::Exclusive => gate
                .try_write_owned()
                .map(|guard| ResourceGuard::Hook { _guard: guard }),
            GuardMode::Shared => gate
                .try_read_owned()
                .map(|guard| ResourceGuard::Operation { _guard: guard }),
        };
        guard.context("workspace resource operation is in progress; retry when its hook finishes")
    }

    /// Hold the PR gates of every workspace in `ids`, which order a
    /// workspace's item changes, links, completion and polls against its
    /// cleanup without making other workspaces wait. Callers lock them
    /// together, in ID order, and never take another while they hold one, so
    /// callers with overlapping sets cannot deadlock.
    pub(crate) async fn lock_prs<S: AsRef<str>>(
        &self,
        ids: impl IntoIterator<Item = S>,
    ) -> PrGuard {
        let mut guards = Vec::new();
        for gate in self.prs.gates(ids) {
            guards.push(gate.lock_owned().await);
        }
        PrGuard { _guards: guards }
    }

    /// [`Self::lock_prs`] when nothing holds any of the gates.
    pub(crate) fn try_lock_prs<S: AsRef<str>>(
        &self,
        ids: impl IntoIterator<Item = S>,
    ) -> Option<PrGuard> {
        let guards = self
            .prs
            .gates(ids)
            .into_iter()
            .map(|gate| gate.try_lock_owned().ok())
            .collect::<Option<_>>()?;
        Some(PrGuard { _guards: guards })
    }

    /// How many PR gates someone holds or awaits.
    #[cfg(test)]
    fn pr_gates_in_use(&self) -> usize {
        self.prs.len()
    }
}
