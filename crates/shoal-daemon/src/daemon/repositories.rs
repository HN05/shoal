//! What the daemon gives the repositories' service: the spaces whose
//! settings it reads, the runner that follows a local config, the forges a
//! contribution forks on, and the workspaces' records it reads.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_forge::account::Account;
use shoal_protocol::space_names::SpaceSelector;
use shoal_repositories::daemon::{Forges, Records, Spaces};

use crate::{
    daemon::manager::Manager,
    forge::ForgeRepo,
    model::{Space, Workspace},
};

impl Spaces for Manager {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space(self, selector))
    }

    fn local_config_changed(&self) {
        // The runner follows `runner.enabled` as soon as it changes.
        self.runner.jobs().settings_changed();
    }
}

impl Forges for Manager {
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::forge(self, remote))
    }

    fn user_account(&self) -> Account {
        self.links.user_account()
    }
}

/// The records of other subsystems the repositories read.
pub(crate) struct RepositoryRecords;

impl Records for RepositoryRecords {
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>> {
        super::workspace::workspaces_within(db, None)
    }
}

#[cfg(test)]
mod tests;
