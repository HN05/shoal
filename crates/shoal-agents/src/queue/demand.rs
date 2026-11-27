//! What each repository asks of this machine, which it reports to the hub
//! for placement to spare a machine that alone serves a repository.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use shoal_daemon_base::{events::agent_time, run_usage};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::time::UnixTime;
use shoal_schedule::placement::snapshot::{DEMAND_WINDOW, Demand};

use super::{room::heavy_entries, store::queued_rows};
use crate::Agents;
use crate::daemon::Daemon;

impl Agents {
    /// What each repository with any asks of this machine: how long its
    /// heavy agents ran and its CI jobs and commands took over the last
    /// [`DEMAND_WINDOW`], and its heavy agents waiting in the queue now.
    pub async fn repository_demand(
        &self,
        daemon: &dyn Daemon,
    ) -> Result<HashMap<RepositoryId, Demand>> {
        let now = UnixTime::now();
        let since = now - DEMAND_WINDOW;
        let (agents, runs, queued) = (self.store)
            .read(move |tx| {
                Ok((
                    agent_time::running_since(tx, since, now)?,
                    run_usage::busy_since(tx, since)?,
                    queued_rows(tx)?,
                ))
            })
            .await?;
        let repositories: HashSet<_> = (agents.keys())
            .map(|(repository, _)| Some(repository.clone()))
            .collect();
        let light = self.light_including(daemon, repositories).await?;
        let heavy_agents = (agents.into_iter())
            .filter(|((repository, mode), _)| !light.mode(Some(repository), mode.as_ref()))
            .map(|((repository, _), ran)| (repository, ran));
        let waiting = heavy_entries(&queued, &light).filter_map(|entry| entry.repository.clone());
        Ok(summed(heavy_agents.chain(runs), waiting))
    }
}

/// The demand of each repository that kept this machine `busy` for the
/// times given, with a heavy agent `waiting` for each time it is named.
fn summed(
    busy: impl IntoIterator<Item = (RepositoryId, Duration)>,
    waiting: impl IntoIterator<Item = RepositoryId>,
) -> HashMap<RepositoryId, Demand> {
    let mut demand: HashMap<RepositoryId, Demand> = HashMap::new();
    for (repository, ran) in busy {
        demand.entry(repository).or_default().busy_seconds += ran.as_secs();
    }
    for repository in waiting {
        demand.entry(repository).or_default().heavy_waiting += 1;
    }
    demand
}
