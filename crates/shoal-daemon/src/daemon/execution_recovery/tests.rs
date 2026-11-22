use super::waits_out;
use anyhow::Context as _;
use std::io::ErrorKind;

fn sqlite(code: i32) -> anyhow::Error {
    let failure = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
    anyhow::Error::new(failure).context("record the stop")
}

#[test]
fn a_full_store_or_disk_sets_a_restore_back_without_cancelling_it() {
    assert!(waits_out(&sqlite(rusqlite::ffi::SQLITE_FULL)));
    assert!(waits_out(&sqlite(rusqlite::ffi::SQLITE_NOMEM)));
    let disk = std::io::Error::from(ErrorKind::StorageFull);
    assert!(waits_out(
        &Err::<(), _>(disk).context("save the record").unwrap_err()
    ));
}

#[test]
fn other_errors_cancel_a_restore() {
    assert!(!waits_out(&sqlite(rusqlite::ffi::SQLITE_BUSY)));
    assert!(!waits_out(&anyhow::anyhow!("execution no longer exists")));
    let denied = std::io::Error::from(ErrorKind::PermissionDenied);
    assert!(!waits_out(&anyhow::Error::new(denied)));
}
