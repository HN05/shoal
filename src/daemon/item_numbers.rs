//! How questions and approval requests are numbered within their space.
//! Both kinds share one sequence per space, so `3.2` names one item: a new
//! one takes the smallest number no item of its space holds.
use anyhow::Result;
use rusqlite::Connection;

pub(crate) use crate::client::item_numbers::ItemNumber;

/// The smallest item number no question or approval request of the space
/// numbered `space` holds.
pub(crate) fn free(db: &Connection, space: u32) -> Result<u32> {
    Ok(db.query_row(
        "WITH held(number) AS (
            SELECT number FROM questions WHERE space_number=?1
            UNION ALL SELECT number FROM access_requests WHERE space_number=?1)
        SELECT MIN(n) FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM held)
            WHERE n NOT IN (SELECT number FROM held)",
        [space],
        |row| row.get(0),
    )?)
}
