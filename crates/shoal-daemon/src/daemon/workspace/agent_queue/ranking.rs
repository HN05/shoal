//! Ranking the queue: the snapshot the rank reads, from the store and from
//! what the daemon keeps in memory, the waiting entries in rank order, and
//! spaces in the order their agents would start.
use std::collections::{HashMap, hash_map};

use anyhow::Result;
use rusqlite::Transaction;

use super::{
    rank::{self, Ranked, Snapshot},
    state::WaitingOneShot,
};
use crate::client::{
    priority::Priority, queue::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId,
    time::UnixTime,
};
use crate::config::{Effective, machine::Schedule};
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
        let repositories = labelled.iter().map(|space| space.repository.clone());
        let settings = self.settings_of(repositories.collect()).await;
        let priorities = spaces::label_priorities(labelled, &settings);
        spaces::prioritize(&mut snapshot.spaces, priorities);
        Ok(snapshot)
    }

    /// The settings of each of `repositories`, none for one whose settings
    /// do not resolve, so that its config, which every command there
    /// reports, holds up no other repository's work.
    async fn settings_of(&self, repositories: Vec<RepositoryId>) -> Settings {
        let mut settings = Settings::new();
        for repository in repositories {
            if let hash_map::Entry::Vacant(vacant) = settings.entry(repository) {
                let resolved = self.repository_settings(vacant.key()).await.ok();
                vacant.insert(resolved);
            }
        }
        settings
    }
}

/// The settings of the repositories ranking reads, none for one whose
/// settings do not resolve.
type Settings = HashMap<RepositoryId, Option<Effective>>;

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
