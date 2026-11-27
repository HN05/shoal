use anyhow::bail;

use super::*;
use crate::{test_support::migrated, write_transaction};

/// Apply `report` in a transaction of its own, counting it in `applied`.
fn apply(db: &mut Connection, report: &ReportId, applied: &mut u32) -> Result<Option<()>> {
    write_transaction(db, |tx| {
        apply_once(tx, report, |_| {
            *applied += 1;
            Ok(())
        })
    })
}

#[test]
fn a_report_sent_again_applies_once() -> Result<()> {
    let mut db = migrated()?;
    let report = ReportId::from("run");
    let mut applied = 0;
    assert_eq!(apply(&mut db, &report, &mut applied)?, Some(()));
    assert_eq!(apply(&mut db, &report, &mut applied)?, None);
    assert_eq!(applied, 1);
    Ok(())
}

#[test]
fn a_report_whose_application_failed_applies_when_sent_again() -> Result<()> {
    let mut db = migrated()?;
    let report = ReportId::from("run");
    let failed = write_transaction(&mut db, |tx| {
        apply_once(tx, &report, |_| -> Result<()> { bail!("disk full") })
    });
    assert!(failed.is_err());
    let mut applied = 0;
    apply(&mut db, &report, &mut applied)?;
    assert_eq!(applied, 1);
    Ok(())
}

#[test]
fn a_report_applied_longer_ago_than_kept_is_forgotten() -> Result<()> {
    let mut db = migrated()?;
    let (old, new) = (ReportId::from("old"), ReportId::from("new"));
    let mut applied = 0;
    apply(&mut db, &old, &mut applied)?;
    db.execute(
        "UPDATE applied_reports SET applied_at = unixepoch() - ?1",
        [KEPT_FOR + 1],
    )?;
    apply(&mut db, &new, &mut applied)?;
    let kept: Vec<String> = db
        .prepare("SELECT id FROM applied_reports")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    assert_eq!(kept, ["new"]);
    Ok(())
}
