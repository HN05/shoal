//! Ranking the queue: the snapshot the rank reads, from the store and from
//! what the daemon keeps in memory, and the waiting entries in rank order.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Transaction;

use super::{
    rank::{self, Ranked, Snapshot},
    state::WaitingOneShot,
};
use crate::client::{priority::Priority, queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
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
        let (mut snapshot, labelled) = (self.store)
            .read(move |tx| {
                Ok((
                    snapshot(tx, &jobs, &oneshots, &leases)?,
                    spaces::labelled(tx)?,
                ))
            })
            .await?;
        let priorities = self.label_priorities(labelled).await?;
        spaces::prioritize(&mut snapshot.spaces, priorities);
        Ok(rank::rank(&snapshot))
    }

    /// The priority each of the `labelled` spaces takes from its issue's
    /// labels under its repository's `[priority.labels]`, where one maps.
    async fn label_priorities(
        &self,
        labelled: Vec<spaces::Labelled>,
    ) -> Result<Vec<(SpaceId, Priority)>> {
        let mut settings = HashMap::new();
        let mut priorities = Vec::new();
        for space in labelled {
            if !settings.contains_key(&space.repository) {
                let resolved = self.repository_settings(&space.repository).await?;
                settings.insert(space.repository.clone(), resolved.priority.clone());
            }
            let labels = space.labels.iter().map(String::as_str);
            if let Some(priority) = settings[&space.repository].of_labels(labels) {
                priorities.push((space.id, priority));
            }
        }
        Ok(priorities)
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
