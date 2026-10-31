//! The leftovers of each space, processes still running after what started
//! them exited, which the cleanup sweep finds. The daemon keeps them in
//! memory, so the first sweep after a restart finds them again, and tells a
//! space's agent about each one once.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Mutex, PoisonError},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{space_processes::chat_space, workspace::Manager},
    process::{described, identity::Identity},
};

/// A leftover as people recognize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftover {
    pub pid: u32,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// Unix seconds when it started.
    pub started_at: u64,
}

/// What the last sweep found, by space.
#[derive(Default)]
pub(crate) struct Leftovers(Mutex<HashMap<String, Found>>);

struct Found {
    leftovers: Vec<(Identity, Leftover)>,
    /// The leftovers the space's agent was told about.
    delivered: HashSet<Identity>,
}

impl Leftovers {
    /// Replace what the last sweep found with `found`, by space. An agent
    /// told about a leftover is not told again while it runs.
    fn record(&self, found: HashMap<String, Vec<(Identity, Leftover)>>) {
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

    /// The leftovers of the workspace or chat `space`.
    pub(crate) fn of(&self, space: &str) -> Vec<Leftover> {
        self.listed(space, |_| true)
    }

    /// The leftovers of `space` its agent has not been told about.
    pub(crate) fn undelivered(&self, space: &str) -> Vec<Leftover> {
        self.listed(space, |found| !found)
    }

    fn listed(&self, space: &str, delivered: impl Fn(bool) -> bool) -> Vec<Leftover> {
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
    pub(crate) fn delivered(&self, space: &str, pids: &[u32]) {
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
        let mut spaces = Vec::new();
        for workspace in self.list_workspaces().await? {
            let worktree = workspace.path.is_dir();
            spaces.push(self.workspace_space(&workspace, worktree).await?);
        }
        spaces.extend(self.chats().await?.iter().map(chat_space));
        let leftovers: Vec<_> = self
            .untracked_processes(spaces, None)
            .await?
            .into_iter()
            .flat_map(|(space, found)| found.into_iter().map(move |found| (space.clone(), found)))
            .filter(|(_, found)| found.leftover)
            .collect();
        let pids: Vec<u32> = leftovers
            .iter()
            .map(|(_, found)| found.identity.pid)
            .collect();
        let described = described::describe(&pids).await?;
        let now = crate::time::unix_seconds();
        let mut found: HashMap<String, Vec<(Identity, Leftover)>> = HashMap::new();
        for (space, process) in leftovers {
            let Some(description) = described.get(&process.identity.pid) else {
                continue;
            };
            let leftover = Leftover {
                pid: process.identity.pid,
                command: description.command.clone(),
                directory: process.directory,
                started_at: now.saturating_sub(description.elapsed),
            };
            found
                .entry(space)
                .or_default()
                .push((process.identity, leftover));
        }
        self.leftovers.record(found);
        Ok(())
    }

    /// Remember that the agent of the workspace `selector` names was told
    /// about the leftovers `pids` name.
    pub(crate) async fn acknowledge_leftovers(&self, selector: &str, pids: &[u32]) -> Result<()> {
        if !pids.is_empty() {
            let workspace = self.workspace(selector).await?;
            self.leftovers.delivered(&workspace.id, pids);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
