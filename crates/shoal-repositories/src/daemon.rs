//! What the repositories need of the rest of the daemon, which implements
//! it: the forge a contribution forks on, and the records of other
//! subsystems they read in their own transactions.
use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_forge::account::Account;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::model::Workspace;

/// The forges registrations reach.
pub trait Forges: Send + Sync {
    /// The repository `remote` names, on the forge its host serves.
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The user's own logins, which a contribution forks with.
    fn user_account(&self) -> Account;
}

/// Everything the repositories need of the daemon.
pub trait Daemon: Forges {}

impl<T> Daemon for T where T: Forges {}

/// The records of other subsystems the repositories read in their own
/// transactions.
pub trait Records: Send + Sync {
    /// Every workspace, by name.
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>>;
}
