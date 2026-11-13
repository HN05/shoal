//! Ranking the queue: the snapshot the rank reads, from the store and from
//! what the daemon keeps in memory, the waiting entries in rank order, and
//! spaces in the order their agents would start.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Transaction;

use super::{
    rank::{self, Ranked, Snapshot},
    state::WaitingOneShot,
};
use crate::client::{priority::Priority, queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::config::machine::Schedule;
use crate::daemon::{approvals::Target, workspace::Manager};

mod edges;
mod entries;
mod spaces;

impl Manager {
    /// The waiting entries, one-shot runs among them, in the order they
    /// start, each with its rank.
    pub(super) async fn ranked_queue(&self) -> Result<Vec<Ranked>> {
        Ok(rank::rank(&self.ranking_snapshot().await?))
    }

    /// The spaces `ids` in the order their agents would start, first
    /// first, each with its priority, as the queue ranks its entries.
    pub(crate) async fn ranked_spaces(&self, ids: &[SpaceId]) -> Result<Vec<(SpaceId, Priority)>> {
        Ok(rank::spaces_in_order(&self.ranking_snapshot().await?, ids))
    }

    /// What ranking reads now.
    async fn ranking_snapshot(&self) -> Result<Snapshot> {
        let jobs = self.waiting_job_spaces().await?;
        let oneshots = self.agent_queue.waiting_oneshots();
        let leases = self.queues.waiters();
        let schedule = self.config().machine.schedule;
        let (mut snapshot, labelled) = (self.store)
            .read(move |tx| {
                Ok((
                    snapshot(tx, &jobs, &oneshots, &leases, schedule)?,
                    spaces::labelled(tx)?,
                ))
            })
            .await?;
        let priorities = self.label_priorities(labelled).await;
        spaces::prioritize(&mut snapshot.spaces, priorities);
        Ok(snapshot)
    }

    /// The priority each of the `labelled` spaces takes from its issue's
    /// labels under its repository's `[priority.labels]`, where one maps. A
    /// repository whose settings do not resolve maps none, so its config,
    /// which every command there reports, holds up no other repository's
    /// work.
    async fn label_priorities(&self, labelled: Vec<spaces::Labelled>) -> Vec<(SpaceId, Priority)> {
        let mut settings = HashMap::new();
        let mut priorities = Vec::new();
        for space in labelled {
            if !settings.contains_key(&space.repository) {
                let resolved = self.repository_settings(&space.repository).await;
                let labels = resolved.ok().map(|resolved| resolved.priority.clone());
                settings.insert(space.repository.clone(), labels);
            }
            let labels = space.labels.iter().map(String::as_str);
            let mapped = settings[&space.repository].as_ref();
            if let Some(priority) = mapped.and_then(|mapped| mapped.of_labels(labels)) {
                priorities.push((space.id, priority));
            }
        }
        priorities
    }
}

/// What ranking reads now, with the spaces each waiting job serves now by
/// its entry's ID in `jobs`, the `oneshots` waiting for admission, the spaces
/// waiting for the `leases` they queue for and the machine's `schedule`.
fn snapshot(
    tx: &Transaction<'_>,
    jobs: &HashMap<QueueEntryId, Vec<SpaceId>>,
    oneshots: &[WaitingOneShot],
    leases: &[(Target, SpaceId)],
    schedule: Schedule,
) -> Result<Snapshot> {
    Ok(Snapshot {
        entries: entries::waiting(tx, jobs, oneshots)?,
        spaces: spaces::ranked(tx)?,
        edges: edges::waits(tx, leases)?,
        now: UnixTime::now(),
        schedule,
    })
}

#[cfg(test)]
mod tests;
