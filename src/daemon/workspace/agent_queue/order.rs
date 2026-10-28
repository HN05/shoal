//! The order of waiting entries, which `agent_queue.seq` keeps across
//! restarts: a move gives the waiting entries' sequence numbers to them in
//! their new order, in one transaction.
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Transaction, params};

use crate::{model::QueuePlace, state::QueueState};

/// `waiting`, entry IDs in queue order, with `moved` placed at `place`.
fn reorder(waiting: &[String], moved: &str, place: &QueuePlace) -> Result<Vec<String>> {
    let mut order: Vec<String> = waiting.iter().filter(|id| *id != moved).cloned().collect();
    let index = match place {
        QueuePlace::Last => order.len(),
        QueuePlace::Position(position) => {
            let last = order.len() + 1;
            ensure!(
                (1..=last).contains(position),
                "position {position} is outside the queue; choose 1 to {last} or last"
            );
            position - 1
        }
        QueuePlace::Before(entry) => order
            .iter()
            .position(|id| id == entry)
            .context("the entry to move in front of is no longer waiting")?,
    };
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

/// An entry's 1-based positions before and after a move.
pub(super) struct Moved {
    pub from: usize,
    pub to: usize,
}

/// Place the waiting entry `moved` at `place`.
pub(super) fn place(tx: &Transaction<'_>, moved: &str, place: &QueuePlace) -> Result<Moved> {
    let rows = waiting(tx)?;
    let (ids, mut seqs): (Vec<String>, Vec<i64>) = rows.into_iter().unzip();
    let position = |order: &[String]| {
        order
            .iter()
            .position(|id| id == moved)
            .map(|index| index + 1)
    };
    let Some(from) = position(&ids) else {
        bail!("the entry is no longer waiting");
    };
    let order = reorder(&ids, moved, place)?;
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
    Ok(Moved {
        from,
        to: position(&order).unwrap_or(from),
    })
}

#[cfg(test)]
mod tests;
