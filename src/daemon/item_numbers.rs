//! How questions and approval requests are numbered within their space.
//! Both kinds share one sequence per space, so `3.2` names one item: a new
//! one takes the smallest number no item of its space holds, and a number
//! names the other kind's item only as a refusal that says so.
use anyhow::Result;
use rusqlite::{Connection, params};

pub(crate) use crate::client::item_numbers::{ItemKind, ItemNumber};
use crate::refusal::{Reason, Refusal};

/// The number of the space `id`, a workspace or a chat.
pub(crate) fn space_number(db: &Connection, id: &str) -> Result<u32> {
    Ok(db.query_row(
        "SELECT number FROM workspaces WHERE id=?1 UNION ALL SELECT number FROM chats WHERE id=?1",
        [id],
        |row| row.get(0),
    )?)
}

/// The next number of the space `id`.
pub(crate) fn next(db: &Connection, id: &str) -> Result<ItemNumber> {
    let space = space_number(db, id)?;
    Ok(ItemNumber {
        space,
        item: free(db, space)?,
    })
}

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

/// The refusal of `number`, which names no item of `kind`: one naming the
/// right command when it names an item of the other kind.
pub(crate) fn unknown(
    db: &Connection,
    kind: ItemKind,
    number: ItemNumber,
) -> Result<anyhow::Error> {
    let other = match kind {
        ItemKind::Question => (ItemKind::Approval, "access_requests"),
        ItemKind::Approval => (ItemKind::Question, "questions"),
    };
    let held = crate::daemon::store::exists(
        db,
        &format!(
            "SELECT 1 FROM {} WHERE space_number=?1 AND number=?2",
            other.1
        ),
        params![number.space, number.item],
    )?;
    let message = match (held, kind) {
        (true, _) => other.0.misdirected(number),
        (false, ItemKind::Question) => format!("unknown question {number}"),
        (false, ItemKind::Approval) => format!("unknown approval request {number}"),
    };
    Ok(Refusal::new(Reason::NotFound, message).into())
}

#[cfg(test)]
mod tests;
