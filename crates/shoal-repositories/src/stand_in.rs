//! A stand-in for the rest of the daemon, which the repositories' tests
//! build the service over: a home with its state directory under a
//! temporary directory, the default global config, no spaces, coordinators,
//! reviews, runner or agents, no files other subsystems keep, and no forge a
//! contribution could fork on.
use shoal_daemon_base::forge_access::ForgeAccess;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::shoal_dirs::ShoalDir;
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, RepositoryRemoval, Space, SyncedRepository, Workspace},
    paths::Paths,
    space_names::SpaceSelector,
    state::WorkspaceState,
};
use shoal_refs::BranchName;

use crate::{
    Repositories,
    daemon::{Activity, Agents, Branches, Followers, Host, Occupants, Records, Spaces},
    moves::{Running, chats::OutsideChat},
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) repositories: Repositories,
    pub(crate) daemon: StandIn,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = shoal_store::test_support::unsynced_store(paths.state.join("state.sqlite3"))
            .await
            .unwrap();
        let config = shoal_core::test_support::global_config("");
        let gates = Arc::new(Gates::default());
        let config = Arc::new(ConfigState::new(config));
        let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
        let repositories = Repositories::new(
            store.clone(),
            gates,
            config,
            Arc::new(Notifications::new(store)),
            Arc::default(),
            Arc::new(forges),
            paths,
            Arc::new(StandIn),
        );
        Self {
            root,
            repositories,
            daemon: StandIn,
        }
    }

    /// The configured `root_dir`.
    pub(crate) fn root_dir(&self) -> std::path::PathBuf {
        let repositories = &self.repositories;
        repositories.config().root_dir(&repositories.paths).unwrap()
    }
}

/// A daemon without spaces or forges.
pub(crate) struct StandIn;

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(async move { bail!("no space {selector}") })
    }

    fn find_space<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(async { Ok(None) })
    }

    fn ensure_removable_workspace<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_movable_workspace<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn record_moved_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn outside_dirs(&self, _: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn outside_chats_moving<'a>(
        &'a self,
        _: &'a Path,
        _: &'a Path,
        _: Running,
    ) -> BoxFuture<'a, Result<Vec<OutsideChat>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn move_outside_chats<'a>(
        &'a self,
        _: Vec<OutsideChat>,
        _: &'a Path,
        _: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn remove_for_repository<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { bail!("no space {id}") })
    }

    fn hold_handovers<'a>(
        &'a self,
        work: BoxFuture<'a, Result<Option<RepositoryRemoval>>>,
    ) -> BoxFuture<'a, Result<Option<RepositoryRemoval>>> {
        work
    }
}

impl Followers for StandIn {
    fn local_config_changed(&self) {}
}

impl Occupants for StandIn {
    fn ensure_no_reviews_in<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_no_import_in(&self, _: ShoalDir) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_reviews<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ensure_no_review_runs_left<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_no_gate_runs_in<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_runner_of_removed<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_kept_files<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Agents for StandIn {
    fn trust_removed<'a>(&'a self, _: &'a Repository, _: Vec<PathBuf>) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn trust_moved<'a>(
        &'a self,
        _: &'a Repository,
        _: Vec<(PathBuf, PathBuf)>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn forget_agent_environments<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Coordinators for StandIn {
    fn tell(&self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'_, Option<Sent>> {
        Box::pin(async { None })
    }
}

impl Branches for StandIn {
    fn fetch_default_remotes<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<BranchName>> {
        Box::pin(async { bail!("no remotes") })
    }

    fn fast_forward_default<'a>(
        &'a self,
        _: &'a Repository,
        _: &'a BranchName,
    ) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(async { bail!("no remotes") })
    }
}

impl Host for StandIn {
    fn clear_of_overload(&self, _: &[&Path]) -> bool {
        true
    }
}

impl Records for StandIn {
    fn workspaces(&self, _: &Connection) -> Result<Vec<Workspace>> {
        Ok(Vec::new())
    }

    fn removal_depth(&self, _: &Connection, _: &SpaceId) -> Result<u32> {
        Ok(0)
    }

    fn ensure_no_external_leases(&self, _: &Connection, _: &RepositoryId) -> Result<()> {
        Ok(())
    }

    fn repository_in_use(&self, _: &Connection, _: &RepositoryId) -> Result<bool> {
        Ok(false)
    }

    fn repository_activity(&self, _: &Connection, _: &RepositoryId) -> Result<Activity> {
        Ok(Activity {
            checkout_leased: false,
            chats: false,
            launches: false,
            workspace_states: Default::default(),
        })
    }

    fn reconcile_workspaces(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }

    fn restore_workspace_state(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
        _: WorkspaceState,
    ) -> Result<()> {
        Ok(())
    }

    fn rebase_paths(
        &self,
        _: &Transaction<'_>,
        _: &RepositoryId,
        _: &Path,
        _: &Path,
    ) -> Result<()> {
        Ok(())
    }

    fn forget_repository_pools(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }
}
