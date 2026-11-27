//! The reports the daemon applied, kept by the ID their sender gave them so
//! that a report sent again applies once, as docs/design/daemon.md describes.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::report_ids::ReportId;

/// How long, in seconds, a report's ID is kept after the daemon applied it:
/// longer than any sender goes on sending a report again.
const KEPT_FOR: i64 = 24 * 60 * 60;

/// Apply the report `report` names with `apply`, in the caller's
/// transaction, unless the daemon applied it before; `None` then. A failure
/// of `apply` must roll the transaction back, so that the report applies
/// when it comes again.
pub fn apply_once<T>(
    db: &Connection,
    report: &ReportId,
    apply: impl FnOnce(&Connection) -> Result<T>,
) -> Result<Option<T>> {
    db.execute(
        "DELETE FROM applied_reports WHERE applied_at < unixepoch() - ?1",
        [KEPT_FOR],
    )?;
    let first = db.execute(
        "INSERT INTO applied_reports(id) VALUES (?1) ON CONFLICT DO NOTHING",
        params![report],
    )?;
    if first == 0 {
        return Ok(None);
    }
    apply(db).map(Some)
}

#[cfg(test)]
mod tests;
