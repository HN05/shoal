//! The daemon's gates: each orders one kind of change against the others
//! that would conflict with it, and is taken through the operation named
//! for it.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::protocol::{ErrorCode, Exit};
use tokio::sync::{
    Mutex, MutexGuard, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
    RwLockReadGuard, watch,
};

use keyed::Keyed;

mod keyed;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct Gates {
    background_operations: Arc<RwLock<()>>,
    /// The exits that wait for work in progress, holding back new work.
    held_back: Arc<watch::Sender<Holds>>,
    /// Whether an exit that waited holds operations off until the daemon
    /// has exited.
    exit_quiet: Arc<watch::Sender<bool>>,
    registry: Mutex<()>,
    checkpoints: Mutex<()>,
    simulators: Mutex<()>,
    proposal_files: Mutex<()>,
    announcements: Mutex<()>,
    git: Keyed<Mutex<()>>,
    creations: Keyed<Mutex<()>>,
    fetches: Keyed<Mutex<()>>,
    message_files: Keyed<Mutex<()>>,
    forge_hosts: Keyed<Mutex<()>>,
    resources: Keyed<RwLock<()>>,
    prs: Keyed<Mutex<()>>,
    /// Signalled when [`Self::try_lock_prs`] finds a gate held, so a test
    /// releases it only once a caller saw it held.
    #[cfg(test)]
    pub(crate) pr_contention: tokio::sync::Notify,
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

/// The daemon holds back new work until it exits: a restart's next daemon
/// takes the request, which [`ErrorCode::Exiting`] tells its client, while
/// a stop leaves none to take it.
#[derive(Debug)]
pub struct Exiting {
    pub exit: Exit,
}

impl Exiting {
    /// The code the daemon refuses a request with.
    pub fn code(&self) -> ErrorCode {
        match self.exit {
            Exit::Restart => ErrorCode::Exiting,
            Exit::Stop => ErrorCode::OperationFailed,
        }
    }
}

impl std::fmt::Display for Exiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.exit {
            Exit::Restart => "the daemon is about to restart and starts no new work; send the request again once it is back",
            Exit::Stop => "the daemon is about to stop and starts no new work; run the command again once a daemon runs",
        })
    }
}

impl std::error::Error for Exiting {}

/// Operations held off for an exit that waited, until it drops.
pub(crate) struct ExitQuiescence {
    exit_quiet: Arc<watch::Sender<bool>>,
    _quiescence: OwnedRwLockWriteGuard<()>,
}

impl Drop for ExitQuiescence {
    fn drop(&mut self) {
        self.exit_quiet.send_replace(false);
    }
}

/// How many exits of each kind hold back new work.
#[derive(Debug, Default, Clone, Copy)]
struct Holds {
    stops: usize,
    restarts: usize,
}

impl Holds {
    fn count(&mut self, exit: Exit) -> &mut usize {
        match exit {
            Exit::Stop => &mut self.stops,
            Exit::Restart => &mut self.restarts,
        }
    }

    /// The exit new work is refused for, a stop before a restart, since no
    /// daemon follows it.
    fn exiting(self) -> Option<Exiting> {
        let exit = match self {
            Holds { stops: 1.., .. } => Exit::Stop,
            Holds { restarts: 1.., .. } => Exit::Restart,
            Holds { .. } => return None,
        };
        Some(Exiting { exit })
    }
}

/// New work held back for an exit that waits, until it drops; work starts
/// again once every such exit has dropped its hold.
pub(crate) struct HeldBack {
    held_back: Arc<watch::Sender<Holds>>,
    exit: Exit,
}

impl Drop for HeldBack {
    fn drop(&mut self) {
        (self.held_back).send_modify(|holds| *holds.count(self.exit) -= 1);
    }
}

impl Gates {
    /// Cover a background operation, so a daemon update can quiesce
    /// operations without cancelling hooks or mutations already in progress.
    pub(crate) async fn operation(&self) -> RwLockReadGuard<'_, ()> {
        self.background_operations.read().await
    }

    /// [`Self::operation`] for a background pass that starts work an exit
    /// would cut off, such as a cleanup sweep, which waits to start while an
    /// exit holds such work back.
    pub(crate) async fn background_pass(&self) -> RwLockReadGuard<'_, ()> {
        let mut held_back = self.held_back.subscribe();
        // The sender lives as long as the gates.
        let _ = held_back.wait_for(|holds| holds.exiting().is_none()).await;
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
    pub(crate) async fn quiesce(&self) -> OwnedRwLockWriteGuard<()> {
        self.background_operations.clone().write_owned().await
    }

    /// [`Self::quiesce`] when no background operation is in progress.
    pub(crate) fn try_quiesce(&self) -> Option<OwnedRwLockWriteGuard<()>> {
        self.background_operations.clone().try_write_owned().ok()
    }

    /// Keep `quiescence` until the daemon has exited, for an exit that
    /// waited, which [`Self::operation_unless_exiting`] tells apart from a
    /// passing attempt to quiesce.
    pub(crate) fn keep_for_exit(&self, quiescence: OwnedRwLockWriteGuard<()>) -> ExitQuiescence {
        self.exit_quiet.send_replace(true);
        ExitQuiescence {
            exit_quiet: self.exit_quiet.clone(),
            _quiescence: quiescence,
        }
    }

    /// [`Self::owned_operation`], or `None` once an exit that waited holds
    /// operations off until the daemon has exited.
    pub(crate) async fn operation_unless_exiting(&self) -> Option<OwnedRwLockReadGuard<()>> {
        let mut exit_quiet = self.exit_quiet.subscribe();
        tokio::select! {
            operation = self.owned_operation() => Some(operation),
            _ = exit_quiet.wait_for(|quiet| *quiet) => None,
        }
    }

    /// Hold back the background passes and requests that start work an
    /// `exit` would cut off, until the guard drops, so that work in progress
    /// can end before the daemon exits.
    pub(crate) fn hold_back(&self, exit: Exit) -> HeldBack {
        (self.held_back).send_modify(|holds| *holds.count(exit) += 1);
        HeldBack {
            held_back: self.held_back.clone(),
            exit,
        }
    }

    /// The exit that holds back new work, when one does.
    pub(crate) fn exiting(&self) -> Option<Exiting> {
        self.held_back.borrow().exiting()
    }

    /// Refuse, as [`Exiting`], to start work while an exit holds it back.
    pub(crate) fn admit(&self) -> Result<(), Exiting> {
        self.exiting().map_or(Ok(()), Err)
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
    pub(crate) async fn lock_git(&self, repository_id: &RepositoryId) -> OwnedMutexGuard<()> {
        self.git.gate(repository_id.as_str()).lock_owned().await
    }

    /// Hold the creation gate of the new workspace `id` until creation
    /// settles it, so that creation can release the repository's Git gate
    /// before it clones the seed. Creation takes it while it still holds the
    /// Git gate under which it recorded the workspace.
    pub(crate) async fn lock_creation(&self, id: &SpaceId) -> OwnedMutexGuard<()> {
        self.creations.gate(id.as_str()).lock_owned().await
    }

    /// Wait until the creation of the workspace `id`, recorded in
    /// `repository_id`, has ended, or return at once when none runs.
    /// Callers must not hold the repository's Git gate.
    pub(crate) async fn wait_for_creation(&self, repository_id: &RepositoryId, id: &SpaceId) {
        // Under the Git gate, creation either has yet to record the
        // workspace or already holds its creation gate.
        let git = self.lock_git(repository_id).await;
        let creation = self.creations.gate(id.as_str());
        drop(git);
        drop(creation.lock().await);
    }

    /// Hold the repository's fetch gate while a sync fetches its remotes, so
    /// syncs of one repository never overlap and moving or removing its
    /// directory waits for the fetch, while commands that take only the Git
    /// gate do not. Callers take it before the Git gate.
    pub(crate) async fn lock_fetch(&self, repository_id: &RepositoryId) -> OwnedMutexGuard<()> {
        self.fetches.gate(repository_id.as_str()).lock_owned().await
    }

    /// [`Self::lock_git`] when nothing holds the gate.
    pub(crate) fn try_lock_git(&self, repository_id: &RepositoryId) -> Option<OwnedMutexGuard<()>> {
        self.git.gate(repository_id.as_str()).try_lock_owned().ok()
    }

    /// The Git gate of `repository_id`, for tests that count who waits on it.
    #[cfg(test)]
    pub(crate) fn git_gate(&self, repository_id: &RepositoryId) -> Arc<Mutex<()>> {
        self.git.gate(repository_id.as_str())
    }

    /// Order keeping the files attached to a repository's agent messages,
    /// from copying them through queueing or discarding the message, against
    /// other sends, workspace removal and moving the repository's directory,
    /// so a queued message names copies that exist where it says. Callers
    /// holding the repository's Git gate take it after that one.
    pub(crate) async fn lock_message_files(
        &self,
        repository_id: &RepositoryId,
    ) -> OwnedMutexGuard<()> {
        self.message_files
            .gate(repository_id.as_str())
            .lock_owned()
            .await
    }

    /// [`Self::lock_message_files`] for the files of the chats without a
    /// repository, whose key no repository ID takes.
    pub(crate) async fn lock_outside_message_files(&self) -> OwnedMutexGuard<()> {
        self.message_files.gate("").lock_owned().await
    }

    /// The attached-files gate of `repository_id`, for tests that count who
    /// waits on it.
    #[cfg(test)]
    pub(crate) fn message_files_gate(&self, repository_id: &RepositoryId) -> Arc<Mutex<()>> {
        self.message_files.gate(repository_id.as_str())
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
    pub(crate) fn resource_guard(&self, id: &SpaceId, mode: GuardMode) -> Result<ResourceGuard> {
        let gate = self.resources.gate(id.as_str());
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
            .collect::<Option<_>>();
        #[cfg(test)]
        if guards.is_none() {
            self.pr_contention.notify_one();
        }
        Some(PrGuard { _guards: guards? })
    }

    /// How many PR gates someone holds or awaits.
    #[cfg(test)]
    pub(crate) fn pr_gates_in_use(&self) -> usize {
        self.prs.len()
    }
}
