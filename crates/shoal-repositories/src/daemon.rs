//! What the repositories need of the rest of the daemon.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::shoal_dirs::ShoalDir;
use shoal_daemon_base::notifications::Coordinators;
use shoal_daemon_base::run_usage::RunUsage;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, RepositoryRemoval, Space, SyncedRepository, Workspace},
    space_names::SpaceSelector,
    state::WorkspaceState,
};
use shoal_refs::BranchName;
use tokio::sync::watch;

use crate::moves::{Running, chats::OutsideChat};

/// The spaces whose settings the repositories' layers resolve, that a
/// repository's removal takes, and that a move of its directory or of
/// `root_dir` carries along.
pub trait Spaces: Send + Sync {
    /// The workspace or chat `selector` names, by ID or name.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// The workspace or chat with the ID `id`, while it exists.
    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>>;

    /// Refuse a workspace its repository's removal cannot take: one busy or
    /// whose worktree is not verifiably its own.
    fn ensure_removable_workspace<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse a workspace its repository's move cannot take: one whose
    /// worktree is not verifiably its own, or was moved.
    fn ensure_movable_workspace<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<()>>;

    /// Verify that `workspace`, whose repository's directory moved, still
    /// owns its worktree, and record its admin directory there.
    fn record_moved_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// The directories in `root` that keep the files of the chats without a
    /// repository, which a move of `root_dir` takes from it.
    fn outside_dirs(&self, root: &Path) -> Vec<PathBuf>;

    /// The chats without a repository whose files or records a move to
    /// `root_dir`, which names or links to `destination`, changes, refusing
    /// one that runs unless `running` has the caller stop it first.
    fn outside_chats_moving<'a>(
        &'a self,
        root_dir: &'a Path,
        destination: &'a Path,
        running: Running,
    ) -> BoxFuture<'a, Result<Vec<OutsideChat>>>;

    /// Move the files of `chats` into `destination`, which `root_dir` names
    /// or links to once the move is done, and record that each runs there.
    fn move_outside_chats<'a>(
        &'a self,
        chats: Vec<OutsideChat>,
        root_dir: &'a Path,
        destination: &'a Path,
    ) -> BoxFuture<'a, Result<()>>;

    /// Remove the space `id` as its repository's removal does.
    fn remove_for_repository<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Run `work`, a repository's removal, while no converted space's agent
    /// is handed over to its new place.
    fn hold_handovers<'a>(
        &'a self,
        work: BoxFuture<'a, Result<Option<RepositoryRemoval>>>,
    ) -> BoxFuture<'a, Result<Option<RepositoryRemoval>>>;
}

/// Those who follow a repository's local config, such as the runner.
pub trait Followers: Send + Sync {
    /// Tell them that a local config changed.
    fn local_config_changed(&self);
}

/// The subsystems that work in a repository's directories or keep files
/// there, which its removal stops or deletes.
pub trait Occupants: Send + Sync {
    /// Refuse while a review runs or waits in the repository `id`.
    fn ensure_no_reviews_in<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>>;

    /// Refuse while an import reads a package into `imports`.
    fn ensure_no_import_in(&self, imports: ShoalDir) -> BoxFuture<'_, Result<()>>;

    /// Stop the reviews running or queued in the repository `id`.
    fn stop_reviews<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, ()>;

    /// Refuse while a review's run left processes in the repository `id`.
    fn ensure_no_review_runs_left<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>>;

    /// Refuse while a gate run works in the repository `id`.
    fn ensure_no_gate_runs_in<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>>;

    /// Delete the runner of the removed `repo`.
    fn delete_runner_of_removed<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Result<()>>;

    /// Delete the files other subsystems keep in `repo`'s directory: its
    /// proposals', imported sessions' and spaces' kept files.
    fn delete_kept_files<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Result<()>>;
}

/// The agents' tools, whose configs trust a repository's paths.
pub trait Agents: Send + Sync {
    /// Drop the trust entries for `repo` and its workspaces at `paths`,
    /// which go with it; whether the agents' configs were read, which
    /// the removal then forgets.
    fn trust_removed<'a>(
        &'a self,
        repo: &'a Repository,
        paths: Vec<PathBuf>,
    ) -> BoxFuture<'a, bool>;

    /// Carry the trust entries for `repo`'s paths that moved, each with the
    /// path it has now.
    fn trust_moved<'a>(
        &'a self,
        repo: &'a Repository,
        moves: Vec<(PathBuf, PathBuf)>,
    ) -> BoxFuture<'a, ()>;

    /// Forget the config locations `repo`'s agents ran with.
    fn forget_agent_environments<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, ()>;
}

/// The default branch of a checkout, which its syncs fetch and fast-forward
/// as a refresh of a branch does.
pub trait Branches: Send + Sync {
    /// Fetch the remotes the default branch of `repo` follows; returns the
    /// default branch. The caller holds the repository's fetch gate.
    fn fetch_default_remotes<'a>(
        &'a self,
        repo: &'a Repository,
    ) -> BoxFuture<'a, Result<BranchName>>;

    /// Fast-forward the local `default` branch of `repo` to its upstream as
    /// fetched. The caller holds the repository's Git gate.
    fn fast_forward_default<'a>(
        &'a self,
        repo: &'a Repository,
        default: &'a BranchName,
    ) -> BoxFuture<'a, Result<SyncedRepository>>;
}

/// The host the daemon runs on.
pub trait Host: Send + Sync {
    /// Whether memory and the disks of `paths` are clear of their overload
    /// warning thresholds, so a seed build or spare adds no load to a
    /// strained host. A failed reading defers it.
    fn clear_of_overload(&self, paths: &[&Path]) -> bool;

    /// Ask the agent queue to admit a build of the seed of `repository` at
    /// the memory its earlier builds estimate, once readings are healthy
    /// too `after_stop`. The admission ends when the returned value drops.
    fn admit_seed_build<'a>(
        &'a self,
        repository: &'a RepositoryId,
        after_stop: bool,
    ) -> BoxFuture<'a, Result<Box<dyn SeedAdmission + 'a>>>;
}

/// A seed build's admission in the agent queue, which it holds while it
/// lives.
pub trait SeedAdmission: Send + Sync {
    /// Resolves once the build may run.
    fn admitted(&mut self) -> BoxFuture<'_, ()>;

    /// Set once protection stopped the build.
    fn stopped(&self) -> watch::Receiver<bool>;

    /// The build holds `memory` bytes now.
    fn using(&self, memory: u64);

    /// The build's step runs as the process group `group`, whose processes
    /// take the build's place in the kernel's OOM scores; none once the
    /// step ended, whose group ID another group may then take.
    fn runs_in(&self, group: Option<u32>);

    /// Keep the build's `run` for the next build's estimate, which a
    /// failure only logs.
    fn ran(&self, run: RunUsage) -> BoxFuture<'_, ()>;
}

pub trait Daemon: Spaces + Followers + Occupants + Agents + Branches + Host + Coordinators {}

impl<T> Daemon for T where
    T: Spaces + Followers + Occupants + Agents + Branches + Host + Coordinators
{
}

/// The records of other subsystems the repositories read and write in
/// their own transactions.
pub trait Records: Send + Sync {
    /// Every workspace, by name.
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>>;

    /// How many spaces the removal of the space `id` takes before it.
    fn removal_depth(&self, db: &Connection, id: &SpaceId) -> Result<u32>;

    /// Refuse while another repository's workspace borrows the checkout of
    /// the repository `id` as a resource.
    fn ensure_no_external_leases(&self, db: &Connection, id: &RepositoryId) -> Result<()>;

    /// Whether anything recorded still uses the repository `id`: a space,
    /// a queued agent, a pending issue proposal, a kept branch, a job
    /// waiting to rerun, an unexpired checkpoint, removal record or removed
    /// space's files, or a resource another repository's workspace borrows.
    fn repository_in_use(&self, db: &Connection, id: &RepositoryId) -> Result<bool>;

    /// What other subsystems record of the use of the repository `id`,
    /// which decides whether its directory may move.
    fn repository_activity(&self, db: &Connection, id: &RepositoryId) -> Result<Activity>;

    /// Mark the workspaces of the repository `id`, whose directory moves,
    /// as reconciling.
    fn reconcile_workspaces(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()>;

    /// Return the workspace `id` to `state`, which it had before its
    /// repository's directory moved, dropping an error a restart recorded
    /// meanwhile once it is ready.
    fn restore_workspace_state(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        state: WorkspaceState,
    ) -> Result<()>;

    /// Point the records of the repository `id` other subsystems keep
    /// inside `source`, its directory that moved, at `destination`.
    fn rebase_paths(
        &self,
        tx: &Transaction<'_>,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()>;

    /// Forget the resource pools of the repository `id`, which goes.
    fn forget_repository_pools(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()>;
}

/// What other subsystems record of a repository's use.
pub struct Activity {
    /// A lease handed its checkout to a caller.
    pub checkout_leased: bool,
    /// A chat runs in it.
    pub chats: bool,
    /// An agent launch, or a CI job other than one waiting for the runner,
    /// is queued for it.
    pub launches: bool,
    /// The state of each of its workspaces.
    pub workspace_states: BTreeMap<SpaceId, WorkspaceState>,
}
