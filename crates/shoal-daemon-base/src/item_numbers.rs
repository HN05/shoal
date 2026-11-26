//! The numbers of threads and approval requests in their space, which
//! share one sequence, as `docs/design/coordination.md` states. A number of
//! the other kind's item is refused, naming the command that takes it.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

use crate::refusal::{Reason, Refusal};
pub use shoal_protocol::item_numbers::{ItemKind, ItemNumber};
use shoal_protocol::space_names::SpaceSelector;

/// The number of the space `id`, a workspace or a chat.
fn space_number(db: &Connection, id: &SpaceId) -> Result<SpaceNumber> {
    Ok(
        db.query_row("SELECT number FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

/// The next number of the space `id`.
pub fn next(db: &Connection, id: &SpaceId) -> Result<ItemNumber> {
    let space = space_number(db, id)?;
    Ok(ItemNumber {
        space,
        item: free(db, space)?,
    })
}

/// The smallest item number no thread or approval request of the space
/// numbered `space` holds.
fn free(db: &Connection, space: SpaceNumber) -> Result<u32> {
    Ok(db.query_row(
        "WITH held(number) AS (
            SELECT number FROM threads WHERE space_number=?1
            UNION ALL SELECT number FROM access_requests WHERE space_number=?1)
        SELECT MIN(n) FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM held)
            WHERE n NOT IN (SELECT number FROM held)",
        [space],
        |row| row.get(0),
    )?)
}

/// The refusal of `number`, which names no item of `kind`: one naming the
/// right command when it names an item of the other kind, and one naming
/// the space when no space here has its number, as another joined
/// machine's may.
pub fn unknown(db: &Connection, kind: ItemKind, number: ItemNumber) -> Result<anyhow::Error> {
    let other = match kind {
        ItemKind::Thread => (ItemKind::Approval, "access_requests"),
        ItemKind::Approval => (ItemKind::Thread, "threads"),
    };
    let held = crate::store::exists(
        db,
        &format!(
            "SELECT 1 FROM {} WHERE space_number=?1 AND number=?2",
            other.1
        ),
        params![number.space, number.item],
    )?;
    let message = match (held, kind) {
        (true, _) => other.0.misdirected(number),
        (false, ItemKind::Thread) => format!("unknown thread {number}"),
        (false, ItemKind::Approval) => format!("unknown approval request {number}"),
    };
    let numbered =
        crate::store::exists(db, "SELECT 1 FROM spaces WHERE number=?1", [number.space])?;
    Ok(match numbered {
        true => Refusal::new(Reason::NotFound, message).into(),
        false => Refusal::absent(&SpaceSelector::Number(number.space), message).into(),
    })
}

#[cfg(test)]
mod tests;
