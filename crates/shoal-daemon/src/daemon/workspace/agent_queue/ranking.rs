//! Ranking the queue: the snapshot the rank reads, from the store and from
//! what the daemon keeps in memory, and the waiting entries in rank order.
use anyhow::Result;
use rusqlite::Transaction;

use super::{
    rank::{self, Ranked, Snapshot},
    state::WaitingOneShot,
};
use crate::client::time::UnixTime;
use crate::daemon::workspace::Manager;

mod edges;
mod entries;
mod spaces;

impl Manager {
    /// The waiting entries, one-shot runs among them, in the order they
    /// start, each with its rank.
    pub(super) async fn ranked_queue(&self) -> Result<Vec<Ranked>> {
        let oneshots = self.agent_queue.waiting_oneshots();
        let snapshot = (self.store).read(move |tx| snapshot(tx, &oneshots)).await?;
        Ok(rank::rank(&snapshot))
    }
}

/// What ranking reads now, with the `oneshots` waiting for a slot.
fn snapshot(tx: &Transaction<'_>, oneshots: &[WaitingOneShot]) -> Result<Snapshot> {
    Ok(Snapshot {
        entries: entries::waiting(tx, oneshots)?,
        spaces: spaces::ranked(tx)?,
        edges: edges::waits(tx)?,
        now: UnixTime::now(),
    })
}

#[cfg(test)]
mod tests;
