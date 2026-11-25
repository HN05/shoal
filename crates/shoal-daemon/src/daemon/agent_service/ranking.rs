//! The snapshot the agent queue's policy ranks: the waiting entries, the
//! spaces with their priorities, labels and areas, and what waits on what,
//! from the store, from what the daemon keeps in memory and from the
//! repositories' settings.
use std::collections::{HashMap, hash_map};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Transaction;
use shoal_agents::daemon::Ranking;
use shoal_agents::queue::state::WaitingOneShot;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::time::UnixTime;
use shoal_schedule::snapshot::Snapshot;

use crate::{
    config::Effective,
    daemon::{approvals::Target, manager::Manager},
};

mod areas;
mod edges;
mod labels;
mod spaces;

impl Manager {
    /// What ranking reads now.
    pub(super) async fn ranking_snapshot(&self) -> Result<Snapshot> {
        let jobs = self.runner.waiting_job_spaces(self).await?;
        let oneshots = self.agents.agent_queue().waiting_oneshots();
        let leases = self.leases.waiters();
        let (mut snapshot, labelled, areas) = (self.store)
            .read(move |tx| {
                Ok((
                    snapshot(tx, &jobs, &oneshots, &leases)?,
                    spaces::labelled(tx)?,
                    areas::labels(tx)?,
                ))
            })
            .await?;
        let repositories = (labelled.iter().map(|space| space.repository.clone()))
            .chain(areas.repositories())
            .collect();
        let settings = self.settings_of(repositories).await;
        let priorities = spaces::label_priorities(labelled, &settings);
        spaces::prioritize(&mut snapshot.spaces, priorities);
        areas::place(&mut snapshot, areas, &settings);
        Ok(snapshot)
    }

    /// The settings of each of `repositories`, none for one whose settings
    /// do not resolve, so that its config, which every command there
    /// reports, holds up no other repository's work.
    async fn settings_of(&self, repositories: Vec<RepositoryId>) -> Settings {
        let mut settings = Settings::new();
        for repository in repositories {
            if let hash_map::Entry::Vacant(vacant) = settings.entry(repository) {
                let resolved = self
                    .repositories
                    .repository_settings(vacant.key())
                    .await
                    .ok();
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
/// its entry's ID in `jobs`, the `oneshots` waiting for admission and the
/// spaces waiting for the `leases` they queue for.
fn snapshot(
    tx: &Transaction<'_>,
    jobs: &HashMap<QueueEntryId, Vec<SpaceId>>,
    oneshots: &[WaitingOneShot],
    leases: &[(Target, SpaceId)],
) -> Result<Snapshot> {
    Ok(Snapshot {
        entries: shoal_agents::queue::ranking::entries::waiting(tx, jobs, oneshots)?,
        spaces: spaces::ranked(tx)?,
        edges: edges::waits(tx, leases)?,
        now: UnixTime::now(),
        occupied: HashMap::new(),
        running: Default::default(),
    })
}

impl Ranking for Manager {
    fn ranking_snapshot(&self) -> BoxFuture<'_, Result<Snapshot>> {
        Box::pin(self.ranking_snapshot())
    }
}

#[cfg(test)]
mod tests;
