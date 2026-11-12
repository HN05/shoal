//! Ranking the queue: the snapshot the rank reads, from the store and from
//! what the daemon keeps in memory, and the waiting entries in rank order.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Transaction;

use super::{
    rank::{self, Ranked, Snapshot},
    state::WaitingOneShot,
};
use crate::client::{queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::daemon::{approvals::Target, workspace::Manager};

mod edges;
mod entries;
mod spaces;

impl Manager {
    /// The waiting entries, one-shot runs among them, in the order they
    /// start, each with its rank.
    pub(super) async fn ranked_queue(&self) -> Result<Vec<Ranked>> {
        let jobs = self.waiting_job_spaces().await?;
        let oneshots = self.agent_queue.waiting_oneshots();
        let leases = self.queues.waiters();
        let snapshot = (self.store)
            .read(move |tx| snapshot(tx, &jobs, &oneshots, &leases))
            .await?;
        Ok(rank::rank(&snapshot))
    }
}

/// What ranking reads now, with the spaces each waiting job serves now by
/// its entry's ID in `jobs`, the `oneshots` waiting for a slot and the
/// spaces waiting for the `leases` they queue for.
fn snapshot(
    tx: &Transaction<'_>,
    jobs: &HashMap<QueueEntryId, Vec<SpaceId>>,
    oneshots: &[WaitingOneShot],
    leases: &[(Target, SpaceId)],
) -> Result<Snapshot> {
    Ok(Snapshot {
        entries: entries::waiting(tx, jobs, oneshots)?,
        spaces: spaces::ranked(tx)?,
        edges: edges::waits(tx, leases)?,
        now: UnixTime::now(),
    })
}

#[cfg(test)]
mod tests;
