//! Fixtures the tests of the store and of the crates over it share.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

use crate::Store;

pub mod queries;
pub mod queue;
pub mod rows;
pub mod seeds;

/// Skip the syncs to disk that carry `db`'s commits through a power loss,
/// which no test outlives: where `TMPDIR`'s disk syncs slowly, they take
/// seconds of a test that migrates a database.
pub fn skip_syncs(db: &Connection) -> rusqlite::Result<()> {
    db.pragma_update(None, "synchronous", "OFF")
}

/// The database at `path` as a test's fixture opens it, with [`skip_syncs`]
/// and the functions the store's triggers call.
pub fn open_database(path: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    skip_syncs(&db)?;
    crate::journal::register(&db)?;
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
    Store::open_with(path, crate::store::Syncs::Skipped).await
}

/// Each row that one of `queries` selects, as the JSON array of its
/// columns, each text or null, sorted.
pub fn text_rows(db: &Connection, queries: &[&str]) -> Result<Vec<String>> {
    let mut rows = Vec::new();
    for query in queries {
        let mut statement = db.prepare(query)?;
        let columns = statement.column_count();
        let mut selected = statement.query([])?;
        while let Some(row) = selected.next()? {
            let values = (0..columns)
                .map(|column| row.get::<_, Option<String>>(column))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.push(serde_json::to_string(&values)?);
        }
    }
    rows.sort();
    Ok(rows)
}
