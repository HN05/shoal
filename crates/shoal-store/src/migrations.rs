//! Schema migrations: the version this build writes and the steps that
//! bring an older database to it. They live apart from the rest of the
//! store so that two changes that each add a migration conflict here.
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

mod alert_attachments;
mod approval_columns;
mod conversion_columns;
mod github_activity;
mod hub_numbers;
mod item_activity_states;
mod journal_shapes;
mod post_files;
mod post_surfaces;
mod review_targets;
mod thread_outcomes;

/// The schema version this build writes.
pub const SCHEMA_VERSION: i64 = 325;

/// The schema version [`FLOOR_RELEASE`] wrote, the oldest a migration
/// starts from. Raising it also replaces the fixture the tests start from.
const FLOOR: i64 = 295;

/// The release that wrote [`FLOOR`], which brings an older database to it.
const FLOOR_RELEASE: &str = "v0.21.0";

/// Runs before a migration's SQL to check existing rows, or to change them
/// where SQL cannot; failure aborts the upgrade.
type Prepare = fn(&Connection) -> Result<()>;

/// A migration: the version it writes, its SQL and what runs before it.
type Migration = (i64, &'static str, Option<Prepare>);

// Append schema changes in version order, each in `migrations/<version>_<name>.sql`;
// startup recovery belongs in quarantine.
pub(super) const MIGRATIONS: &[Migration] = &[
    (
        296,
        include_str!("migrations/0296_review_held_reopens.sql"),
        None,
    ),
    (
        297,
        include_str!("migrations/0297_review_submits.sql"),
        None,
    ),
    (
        298,
        include_str!("migrations/0298_agent_handoffs.sql"),
        None,
    ),
    (299, include_str!("migrations/0299_run_usage.sql"), None),
    (300, include_str!("migrations/0300_post_surfaces.sql"), None),
    (
        301,
        include_str!("migrations/0301_drop_post_choices.sql"),
        Some(post_surfaces::rewrite_choices),
    ),
    (
        302,
        include_str!("migrations/0302_github_activity_ids.sql"),
        Some(github_activity::key_by_database_ids),
    ),
    (
        303,
        include_str!("migrations/0303_journal_event_shapes.sql"),
        Some(journal_shapes::rewrite_records),
    ),
    (
        304,
        include_str!("migrations/0304_journal_append.sql"),
        None,
    ),
    (
        305,
        include_str!("migrations/0305_journal_columns.sql"),
        None,
    ),
    (
        306,
        include_str!("migrations/0306_seed_build_runs.sql"),
        None,
    ),
    (
        307,
        include_str!("migrations/0307_applied_reports.sql"),
        None,
    ),
    (
        308,
        include_str!("migrations/0308_placement_lifetime.sql"),
        None,
    ),
    (
        309,
        include_str!("migrations/0309_review_target_urls.sql"),
        None,
    ),
    (
        310,
        include_str!("migrations/0310_review_target_columns.sql"),
        Some(review_targets::move_urls),
    ),
    (
        311,
        include_str!("migrations/0311_item_activity_states.sql"),
        None,
    ),
    (
        312,
        include_str!("migrations/0312_item_activity_state_columns.sql"),
        Some(item_activity_states::move_states),
    ),
    (
        313,
        include_str!("migrations/0313_conversion_columns.sql"),
        None,
    ),
    (
        314,
        include_str!("migrations/0314_conversion_records.sql"),
        Some(conversion_columns::move_into_columns),
    ),
    (
        315,
        include_str!("migrations/0315_alert_attachments.sql"),
        None,
    ),
    (
        316,
        include_str!("migrations/0316_drop_alert_attachments_column.sql"),
        Some(alert_attachments::move_paths),
    ),
    (
        317,
        include_str!("migrations/0317_hub_number_columns.sql"),
        None,
    ),
    (
        318,
        include_str!("migrations/0318_drop_hub_number_json.sql"),
        Some(hub_numbers::move_numbers),
    ),
    (
        319,
        include_str!("migrations/0319_approval_columns.sql"),
        None,
    ),
    (
        320,
        include_str!("migrations/0320_approval_records.sql"),
        Some(approval_columns::move_queried_fields),
    ),
    (
        321,
        include_str!("migrations/0321_thread_outcome_columns.sql"),
        None,
    ),
    (
        322,
        include_str!("migrations/0322_thread_outcome_kinds.sql"),
        Some(thread_outcomes::split_outcomes),
    ),
    (
        323,
        include_str!("migrations/0323_thread_post_files.sql"),
        None,
    ),
    (
        324,
        include_str!("migrations/0324_drop_post_file_lists.sql"),
        Some(post_files::list_post_files),
    ),
    (
        325,
        include_str!("migrations/0325_agent_events_by_time.sql"),
        None,
    ),
];

/// The migrations that rebuild a table others reference, which run on their
/// own with foreign keys off, so dropping the old table deletes nothing that
/// references it.
const REBUILDS: &[i64] = &[];

/// The schema the migrations up to [`SCHEMA_VERSION`] make. A new database
/// takes it in one step: SQLite checks the whole schema on every
/// `ALTER TABLE`, so applying each migration in turn takes a good part of a
/// second on a busy machine. The tests check that it matches the
/// migrations, and say how to write it anew.
const SCHEMA: &str = include_str!("migrations/schema.sql");

pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    // A migration's writes can fire the triggers that record events.
    crate::journal::register(db)?;
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
