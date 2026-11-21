//! A stand-in for the rest of the daemon, which the repositories' tests
//! build the service over: a home with its state directory under a
//! temporary directory, the default global config, no spaces or
//! coordinators, and no forge a contribution could fork on.
use std::sync::Arc;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_forge::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::{
    model::{Space, Workspace},
    paths::Paths,
    space_names::SpaceSelector,
};

use crate::{
    Repositories,
    daemon::{Forges, Records, Spaces},
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

    fn local_config_changed(&self) {}
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
}
