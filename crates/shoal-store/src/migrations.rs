//! Schema migrations: the version this build writes and the steps that
//! bring an older database to it, apart from the rest of the store, so that
//! two changes adding a migration both change this file.
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

/// Schema version written by this build; older databases are migrated on open.
pub const SCHEMA_VERSION: i64 = 298;

/// The schema version [`FLOOR_RELEASE`] wrote, the oldest a migration
/// starts from. Raising it to a later release's deletes the migrations at or
/// below it with their tests and replaces the fixture they start from.
const FLOOR: i64 = 295;

/// The release that wrote [`FLOOR`], which brings an older database to it.
const FLOOR_RELEASE: &str = "v0.21.0";

/// Runs before a migration's SQL to check existing rows, or to change them
/// where SQL cannot; failure aborts the upgrade.
type Prepare = fn(&Connection) -> Result<()>;

/// A migration: the version it writes, its SQL and what runs before it.
type Migration = (i64, &'static str, Option<Prepare>);

// Append schema changes in version order; startup recovery belongs in quarantine.
pub(super) const MIGRATIONS: &[Migration] = &[
    (296, include_str!("review_held_reopens.sql"), None),
    (297, include_str!("review_submits.sql"), None),
    (298, include_str!("agent_handoffs.sql"), None),
];

/// The migrations that rebuild a table others reference, which run on their
/// own with foreign keys off, so dropping the old table deletes nothing that
/// references it.
const REBUILDS: &[i64] = &[];

/// The schema the migrations up to [`SCHEMA_VERSION`] make, which a new
/// database takes in one step: SQLite checks the whole schema on every
/// `ALTER TABLE`, so applying each migration in turn takes a new database a
/// good part of a second on a busy machine. The tests check that it is what
/// the migrations make, and say how to write it anew.
const SCHEMA: &str = include_str!("schema.sql");

pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    if is_new(db)? {
        return create_current(db);
    }
    let version = migratable_version(db)?;
    for batch in batches_after(version) {
        apply_batch(db, &batch)?;
    }
    Ok(())
}

/// Whether `db` holds nothing yet.
fn is_new(db: &Connection) -> Result<bool> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let objects: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))?;
    Ok(version == 0 && objects == 0)
}

/// Give the new database `db` the current [`SCHEMA`].
fn create_current(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(SCHEMA)
        .context("create the current schema")?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// The schema version of `db`, refused unless the migrations take it from
/// there: one below [`FLOOR`] is left to [`FLOOR_RELEASE`], and a newer
/// version's to that version.
fn migratable_version(db: &Connection) -> Result<i64> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        version >= FLOOR,
        "the state database was written by a Shoal version older than {FLOOR_RELEASE}, \
         whose schema this version migrates from; start Shoal {FLOOR_RELEASE} once \
         before this version"
    );
    ensure!(
        version <= SCHEMA_VERSION,
        "state database was written by a newer Shoal version"
    );
    Ok(version)
}

/// The migrations after `version`, in batches that each share a
/// transaction; a rebuild runs on its own.
fn batches_after(version: i64) -> Vec<Vec<&'static Migration>> {
    let pending = MIGRATIONS.iter().filter(|(target, ..)| *target > version);
    let mut batches: Vec<Vec<&Migration>> = Vec::new();
    for migration in pending {
        match batches.last_mut() {
            Some(batch)
                if !REBUILDS.contains(&migration.0)
                    && !batch.iter().any(|(target, ..)| REBUILDS.contains(target)) =>
            {
                batch.push(migration)
            }
            _ => batches.push(vec![migration]),
        }
    }
    batches
}

/// Apply `batch`, with foreign keys off while it rebuilds a table.
fn apply_batch(db: &mut Connection, batch: &[&Migration]) -> Result<()> {
    let rebuilds = batch.iter().any(|(target, ..)| REBUILDS.contains(target));
    if rebuilds {
        db.pragma_update(None, "foreign_keys", false)?;
    }
    let applied = apply(db, batch, rebuilds);
    if rebuilds {
        db.pragma_update(None, "foreign_keys", true)?;
    }
    applied
}

/// Apply `batch` in one transaction, which a rebuild leaves only when every
/// foreign key still holds.
fn apply(db: &mut Connection, batch: &[&Migration], rebuilds: bool) -> Result<()> {
    let tx = db.transaction()?;
    for &&(target, sql, prepare) in batch {
        if let Some(prepare) = prepare {
            prepare(&tx).with_context(|| format!("prepare schema migration {target}"))?;
        }
        tx.execute_batch(sql)
            .with_context(|| format!("apply schema migration {target}"))?;
        tx.pragma_update(None, "user_version", target)?;
    }
    if rebuilds {
        let violations: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        ensure!(
            violations == 0,
            "a rebuilt table broke {violations} references"
        );
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests;
