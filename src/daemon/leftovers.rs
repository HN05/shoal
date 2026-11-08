//! The leftovers of each space, processes still running after what started
//! them exited, which the cleanup sweep finds. The daemon keeps them in
//! memory, so the first sweep after a restart finds them again, and tells a
//! space's agent about each one once.
use crate::client::space_ids::SpaceId;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, PoisonError},
};

use anyhow::Result;

pub(crate) use crate::client::processes::Leftover;
use crate::client::{space_names::SpaceSelector, time::UnixTime};
use crate::{
    daemon::{space_processes::WorktreeTies, workspace::Manager},
    fsutil::off_runtime,
    process::{
        described,
        identity::{
            Identity,
            spaces::{Space, Untracked},
        },
    },
};

/// The most leftovers one delivery or status names; the rest follow in
/// later deliveries.
const LISTED_AT_ONCE: usize = 10;

/// The longest command a leftover keeps, so a delivery stays small.
const COMMAND_LIMIT: usize = 300;

/// What the last sweep found, by space.
#[derive(Default)]
pub(crate) struct Leftovers(Mutex<HashMap<SpaceId, Found>>);

struct Found {
    leftovers: Vec<(Identity, Leftover)>,
    /// The leftovers the space's agent was told about.
    delivered: HashSet<Identity>,
}

impl Leftovers {
    /// Replace what the last sweep found with `found`, by space. An agent
    /// told about a leftover is not told again while it runs.
    fn record(&self, found: HashMap<SpaceId, Vec<(Identity, Leftover)>>) {
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

    /// How many leftovers the workspace or chat `space` has, and as many of
    /// them as one status lists.
    pub(crate) fn of(&self, space: &SpaceId) -> (usize, Vec<Leftover>) {
        let mut leftovers = self.listed(space, |_| true);
        let count = leftovers.len();
        leftovers.truncate(LISTED_AT_ONCE);
        (count, leftovers)
    }

    /// The leftovers of `space` its agent has not been told about, as many
    /// as one delivery names in `room` bytes; the rest wait for a later one.
    pub(crate) fn undelivered(&self, space: &SpaceId, room: usize) -> Result<Vec<Leftover>> {
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

    /// Remember that the agent of `space` was told about the leftovers
    /// `pids` name.
    pub(crate) fn delivered(&self, space: &SpaceId, pids: &[u32]) {
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

impl Manager {
    /// Find the leftovers of every workspace and running chat from one
    /// inventory of this user's processes.
    pub(crate) async fn find_leftovers(&self) -> Result<()> {
        let spaces = self.surveyed_spaces().await?;
        let found = {
            let _handoffs = self.chat_handoffs.read().await;
            self.untracked_processes(spaces, None).await?
        };
        self.leftovers.record(described_leftovers(found).await?);
        Ok(())
    }

    /// What ties processes to each workspace and running chat.
    async fn surveyed_spaces(&self) -> Result<Vec<Space>> {
        let mut spaces = Vec::new();
        for workspace in self.list_workspaces().await? {
            let ties = WorktreeTies::when(off_runtime::is_dir(&workspace.path).await?);
            spaces.push(self.workspace_space(&workspace, ties).await?);
        }
        for chat in self.chats().await? {
            spaces.push(self.chat_space(&chat).await?);
        }
        Ok(spaces)
    }

    /// Remember that the agent of the workspace or running chat `selector`
    /// names was told about the leftovers `pids` name.
    pub(crate) async fn acknowledge_leftovers(
        &self,
        selector: &SpaceSelector,
        pids: &[u32],
    ) -> Result<()> {
        if !pids.is_empty() {
            let space = self.space(selector).await?;
            self.leftovers.delivered(space.id(), pids);
        }
        Ok(())
    }
}

/// The leftovers among the untracked processes `found` of each space, as
/// people recognize them, leaving out those that exited meanwhile.
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
    let described = described::describe(&pids).await?;
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

/// Operations on the client type [`Leftover`] that only the `shoal` command uses.
trait LeftoverExt {
    /// `process` as `description` shows it at `now`.
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
