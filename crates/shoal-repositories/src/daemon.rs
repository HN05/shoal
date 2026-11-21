//! What the repositories need of the rest of the daemon, which implements
//! it: the spaces whose settings they read, those who follow the local
//! config, the coordinators alerts about a space reach, the forge a
//! contribution forks on, and the records of other subsystems they read in
//! their own transactions.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_daemon_base::notifications::Coordinators;
use shoal_forge::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::{
    model::{Space, Workspace},
    space_names::SpaceSelector,
};

/// The spaces whose settings the repositories' layers resolve.
pub trait Spaces: Send + Sync {
    /// The workspace or chat `selector` names, by ID or name.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// Tell those that follow a repository's local config, such as the
    /// runner, that it changed.
    fn local_config_changed(&self);
}

/// The forges registrations reach.
pub trait Forges: Send + Sync {
    /// The repository `remote` names, on the forge its host serves.
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The user's own logins, which a contribution forks with.
    fn user_account(&self) -> Account;
}

/// Everything the repositories need of the daemon.
pub trait Daemon: Spaces + Forges + Coordinators {}

impl<T> Daemon for T where T: Spaces + Forges + Coordinators {}

/// The records of other subsystems the repositories read in their own
/// transactions.
pub trait Records: Send + Sync {
    /// Every workspace, by name.
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>>;
}
