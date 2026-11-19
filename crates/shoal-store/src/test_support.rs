//! Fixtures the tests of the store and of the crates over it share.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

use crate::Store;

pub mod queries;

/// The schema of version 0.17.0, the oldest database a migration starts
/// from.
pub const SCHEMA_V17: &str = include_str!("../tests/fixtures/schema_v17.sql");

/// The migration that records who acquired each resource lease, which a
/// test of leases applies to a database of [`SCHEMA_V17`].
pub const LEASE_ACQUIRERS: &str = include_str!("lease_acquirers.sql");

/// Skip the syncs to disk that carry `db`'s commits through a power loss,
/// which no test outlives: where `TMPDIR`'s disk syncs slowly, they take
/// seconds of a test that migrates a database.
pub fn skip_syncs(db: &Connection) -> rusqlite::Result<()> {
    db.pragma_update(None, "synchronous", "OFF")
}

/// The database at `path` as a test's fixture opens it, with [`skip_syncs`].
pub fn open_database(path: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    skip_syncs(&db)?;
    Ok(db)
}

/// A database in memory with the current schema.
pub fn migrated() -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    crate::migrations::migrate(&mut db)?;
    Ok(db)
}

/// The store at `path`, as [`Store::open`] opens it but with
/// [`skip_syncs`] on its connection.
pub async fn unsynced_store(path: PathBuf) -> Result<Store> {
    Store::open_with(path, crate::Syncs::Skipped).await
}
