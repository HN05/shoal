//! The leftovers of each space: processes still running after what started
//! them exited. The daemon keeps them in memory, so the first sweep after a
//! restart finds them again, and tells a space's agent about each one once.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, PoisonError},
};

use anyhow::Result;

use crate::{
    fsutil::off_runtime,
    process::{
        described,
        identity::{
            Identity,
            spaces::{Space, Untracked},
        },
    },
    space_processes::WorktreeTies,
};
pub use shoal_protocol::processes::Leftover;
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};

/// The most leftovers one delivery or status names. The rest follow in
/// later deliveries.
pub const LISTED_AT_ONCE: usize = 10;

pub use shoal_protocol::events::COMMAND_LIMIT;

#[derive(Default)]
pub struct Leftovers(Mutex<HashMap<SpaceId, Found>>);

struct Found {
    leftovers: Vec<(Identity, Leftover)>,
    delivered: HashSet<Identity>,
}

impl Leftovers {
    /// An agent told about a leftover is not told again while it runs.
    pub fn record(&self, found: HashMap<SpaceId, Vec<(Identity, Leftover)>>) {
        let mut spaces = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut earlier = std::mem::take(&mut *spaces);
        for (space, leftovers) in found {
            let mut delivered = earlier
                .remove(&space)
                .map(|found| found.delivered)
                .unwrap_or_default();
            delivered.retain(|told| leftovers.iter().any(|(identity, _)| identity == told));
            spaces.insert(
                space,
                Found {
                    leftovers,
                    delivered,
                },
            );
        }
    }

    /// Returns how many there are, and as many as one status lists.
    pub fn of(&self, space: &SpaceId) -> (usize, Vec<Leftover>) {
        let mut leftovers = self.listed(space, |_| true);
        let count = leftovers.len();
        leftovers.truncate(LISTED_AT_ONCE);
        (count, leftovers)
    }

    /// As many as one delivery names in `room` bytes. The rest wait for a
    /// later one.
    pub fn undelivered(&self, space: &SpaceId, room: usize) -> Result<Vec<Leftover>> {
        let mut used = 0;
        let mut fitting = Vec::new();
        for leftover in self.listed(space, |found| !found) {
            used += serde_json::to_vec(&leftover)?.len() + 1;
            if used > room || fitting.len() == LISTED_AT_ONCE {
                break;
            }
            fitting.push(leftover);
        }
        Ok(fitting)
    }

    fn listed(&self, space: &SpaceId, delivered: impl Fn(bool) -> bool) -> Vec<Leftover> {
        let spaces = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(found) = spaces.get(space) else {
            return Vec::new();
        };
        found
            .leftovers
            .iter()
            .filter(|(identity, _)| delivered(found.delivered.contains(identity)))
            .map(|(_, leftover)| leftover.clone())
            .collect()
    }

    pub fn delivered(&self, space: &SpaceId, pids: &[u32]) {
        let mut spaces = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = spaces.get_mut(space) {
            let told = found
                .leftovers
                .iter()
                .filter(|(_, leftover)| pids.contains(&leftover.pid))
                .map(|(identity, _)| identity.clone())
                .collect::<Vec<_>>();
            found.delivered.extend(told);
        }
    }
}

impl Spaces {
    /// Reads one inventory of this user's processes for all of them.
    pub async fn find_leftovers(&self, daemon: &dyn Daemon) -> Result<()> {
        let spaces = self.surveyed_spaces(daemon).await?;
        let found = self.untracked_processes(spaces, None).await?;
        self.leftovers.record(described_leftovers(found).await?);
        Ok(())
    }

    async fn surveyed_spaces(&self, daemon: &dyn Daemon) -> Result<Vec<Space>> {
        let mut surveyed = Vec::new();
        for space in self.present_spaces(daemon).await? {
            let ties = match space.worktree() {
                Some(worktree) => WorktreeTies::when(off_runtime::is_dir(&worktree.path).await?),
                None => WorktreeTies::None,
            };
            surveyed.push(self.space_ties(daemon, &space, ties).await?);
        }
        Ok(surveyed)
    }

    pub async fn acknowledge_leftovers(
        &self,
        selector: &SpaceSelector,
        pids: &[u32],
    ) -> Result<()> {
        if !pids.is_empty() {
            let space = self.space(selector).await?;
            self.leftovers.delivered(&space.id, pids);
        }
        Ok(())
    }
}

/// Describes each as people recognize it, and leaves out those that exited
/// meanwhile.
async fn described_leftovers(
    found: HashMap<SpaceId, Vec<Untracked>>,
) -> Result<HashMap<SpaceId, Vec<(Identity, Leftover)>>> {
    let leftovers: Vec<(SpaceId, Untracked)> = found
        .into_iter()
        .flat_map(|(space, found)| {
            found
                .into_iter()
                .map(move |process| (space.clone(), process))
        })
        .filter(|(_, process)| process.leftover)
        .collect();
    let pids: Vec<u32> = leftovers
        .iter()
        .map(|(_, process)| process.identity.pid)
        .collect();
    let described = described::describe(&pids)?;
    let now = UnixTime::now();
    let mut by_space: HashMap<SpaceId, Vec<(Identity, Leftover)>> = HashMap::new();
    for (space, process) in leftovers {
        if let Some(description) = described.get(&process.identity.pid) {
            let leftover = Leftover::of(&process, description, now);
            by_space
                .entry(space)
                .or_default()
                .push((process.identity, leftover));
        }
    }
    Ok(by_space)
}

/// Operations on the client type [`Leftover`] that only the `shoal` command
/// uses.
trait LeftoverExt {
    fn of(process: &Untracked, description: &described::Description, now: UnixTime) -> Self;
}

impl LeftoverExt for Leftover {
    fn of(process: &Untracked, description: &described::Description, now: UnixTime) -> Self {
        Self {
            pid: process.identity.pid,
            command: description.command.chars().take(COMMAND_LIMIT).collect(),
            directory: process.directory.clone(),
            started_at: now - std::time::Duration::from_secs(description.elapsed),
        }
    }
}

#[cfg(test)]
mod tests;
