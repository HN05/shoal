use super::*;
use crate::daemon::store::migrations::{migrate, migrate_to};

/// A database in a state directory of its own, migrated to the schema
/// before the one that drops what clears earlier releases' state files.
fn before_dropping_earlier_state() -> Result<(tempfile::TempDir, Connection)> {
    let state = tempfile::tempdir()?;
    let mut db = crate::daemon::test_support::open_database(state.path().join("state.db"))?;
    migrate_to(&mut db, 208)?;
    Ok((state, db))
}

#[test]
fn files_earlier_releases_left_in_the_state_directory_refuse_the_upgrade() -> Result<()> {
    let (state, mut db) = before_dropping_earlier_state()?;
    for (directory, ..) in EARLIER_STATE {
        std::fs::create_dir(state.path().join(directory))?;
        let refused = migrate(&mut db).expect_err("refused");
        assert!(format!("{refused:#}").contains(directory), "{refused:#}");
        std::fs::remove_dir(state.path().join(directory))?;
    }
    migrate(&mut db)
}

#[test]
fn an_unread_notice_of_moved_packages_refuses_the_upgrade_until_read() -> Result<()> {
    let (_state, mut db) = before_dropping_earlier_state()?;
    db.execute_batch(
        "INSERT INTO notifications(created_at,kind,message) VALUES (1,'unmoved_packages','moved')",
    )?;
    assert!(migrate(&mut db).is_err());
    db.execute_batch("UPDATE notifications SET read=1")?;
    migrate(&mut db)?;
    let left: i64 = db.query_row("SELECT COUNT(*) FROM notifications", [], |row| row.get(0))?;
    assert_eq!(left, 0);
    Ok(())
}
