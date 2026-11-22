//! A stand-in for the rest of the daemon, which the repositories' tests
//! build the service over: a home with its state directory under a
//! temporary directory, the default global config, no spaces, coordinators,
//! reviews, runner or agents, no files other subsystems keep, and no forge a
//! contribution could fork on.
use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_forge::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, RepositoryRemoval, Space, Workspace},
    paths::Paths,
    space_names::SpaceSelector,
};

use crate::{
    Repositories,
    daemon::{Agents, Forges, Occupants, Records, Spaces},
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
        let repositories = Repositories::new(
            store.clone(),
            Arc::new(Gates::default()),
            Arc::new(ConfigState::new(config)),
            Arc::new(Notifications::new(store)),
            Arc::default(),
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

    fn remove_for_repository<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { bail!("no space {id}") })
    }

    fn hold_handovers<'a>(
        &'a self,
        work: BoxFuture<'a, Result<Option<RepositoryRemoval>>>,
    ) -> BoxFuture<'a, Result<Option<RepositoryRemoval>>> {
        work
    }

    fn local_config_changed(&self) {}
}

impl Occupants for StandIn {
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

    fn remove_seed<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<()>> {
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

    fn forget_agent_environments<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Coordinators for StandIn {
    fn tell(&self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'_, Option<Sent>> {
        Box::pin(async { None })
    }
}

impl Forges for StandIn {
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { bail!("no forge serves {remote}") })
    }

    fn user_account(&self) -> Account {
        Account::default()
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

    fn forget_repository_pools(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }
}
