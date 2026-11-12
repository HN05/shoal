//! The order of waiting entries, which `agent_queue.seq` keeps across
//! restarts: placing an entry in front of another gives the waiting entries'
//! sequence numbers to them in their new order, in one transaction.
use anyhow::{Context as _, Result};
use rusqlite::{Transaction, params};

use crate::client::queue::QueueEntryId;
use crate::state::QueueState;

/// `waiting`, entry IDs in queue order, with `moved` in front of `before`.
fn reorder(waiting: &[String], moved: &str, before: &str) -> Result<Vec<String>> {
    let mut order: Vec<String> = waiting.iter().filter(|id| *id != moved).cloned().collect();
    let index = order
        .iter()
        .position(|id| id == before)
        .context("the entry to place in front of is no longer waiting")?;
    order.insert(index, moved.to_owned());
    Ok(order)
}

/// The waiting entries' IDs and sequence numbers, in queue order.
fn waiting(tx: &Transaction<'_>) -> Result<Vec<(String, i64)>> {
    Ok(tx
        .prepare("SELECT id,seq FROM agent_queue WHERE state=?1 ORDER BY seq")?
        .query_map([QueueState::Queued], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Place the waiting entry `id` in front of the waiting entry `before`
/// within `tx`.
pub(crate) fn place_entry(
    tx: &Transaction<'_>,
    id: &QueueEntryId,
    before: &QueueEntryId,
) -> Result<()> {
    let rows = waiting(tx)?;
    let (ids, mut seqs): (Vec<String>, Vec<i64>) = rows.into_iter().unzip();
    let order = reorder(&ids, id.as_str(), before.as_str())?;
    seqs.sort_unstable();
    // Free every number first, since each must stay unique.
    for (id, seq) in ids.iter().zip(&seqs) {
        tx.execute(
            "UPDATE agent_queue SET seq=?2 WHERE id=?1",
            params![id, -seq],
        )?;
    }
    for (id, seq) in order.iter().zip(&seqs) {
        tx.execute(
            "UPDATE agent_queue SET seq=?2 WHERE id=?1",
            params![id, seq],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
