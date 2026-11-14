//! Which execution's agent a process runs under. An agent's execution is its
//! own wherever the process that started its wrapper runs, so a process below
//! that wrapper is never the work of the execution that started it. A process
//! that hides its environment, as a platform binary does on macOS, takes the
//! marker of the nearest ancestor that tells, which for an agent's process
//! can be its wrapper, started with the marker of the execution that started
//! it; only its place in the tree tells whose it is.
use super::{Identity, alive, same_user_processes};
use anyhow::Result;
use std::collections::HashMap;

/// The live wrappers of the executions that run agents, and the parent of
/// each process this user runs, read at once.
#[derive(Default)]
pub struct AgentTrees {
    wrappers: HashMap<u32, Identity>,
    parents: HashMap<u32, u32>,
}

impl AgentTrees {
    /// The trees below the live ones among `wrappers`.
    pub async fn of(wrappers: &[Identity]) -> Result<Self> {
        let mut live = HashMap::new();
        for wrapper in wrappers {
            if alive(wrapper)? {
                live.insert(wrapper.pid, wrapper.clone());
            }
        }
        if live.is_empty() {
            return Ok(Self::default());
        }
        let rows = same_user_processes().await?;
        Ok(Self {
            wrappers: live,
            parents: HashMap::new(),
        }
        .as_listed(&rows))
    }

    /// The same wrappers' trees as `rows`, a listing of this user's
    /// processes as (PID, process group, parent PID), shows them, so a
    /// process that listing shows is found below its wrapper.
    pub(crate) fn as_listed(&self, rows: &[(u32, u32, u32)]) -> Self {
        Self {
            wrappers: self.wrappers.clone(),
            parents: (rows.iter())
                .map(|(pid, _, parent)| (*pid, *parent))
                .collect(),
        }
    }

    /// The agent wrappers other than `own`, the wrapper of the execution
    /// whose processes are sought.
    pub(crate) fn others(&self, own: Option<&Identity>) -> Vec<Identity> {
        (self.wrappers.values())
            .filter(|wrapper| own != Some(*wrapper))
            .cloned()
            .collect()
    }

    /// The processes that agents other than `own`, the wrapper of the
    /// execution whose processes are sought, run.
    pub(crate) fn besides(&self, own: Option<&Identity>) -> Besides<'_> {
        Besides {
            trees: self,
            own: own
                .filter(|own| alive(own).unwrap_or(false))
                .map(|own| own.pid),
        }
    }
}

/// The processes that agents other than one execution's run.
pub(crate) struct Besides<'a> {
    trees: &'a AgentTrees,
    /// The PID of the execution's live wrapper.
    own: Option<u32>,
}

impl Besides<'_> {
    /// Whether the process `pid` is, or runs below, an agent wrapper other
    /// than the execution's that is nearer to it than the execution's. An
    /// agent a scoped caller runs in its own terminal runs below the caller's
    /// wrapper too, and its processes stay its own.
    pub(crate) fn run(&self, pid: u32) -> bool {
        let mut current = pid;
        for _ in 0..=self.trees.parents.len() {
            if self.own == Some(current) {
                return false;
            }
            if self.trees.wrappers.contains_key(&current) {
                return true;
            }
            match self.trees.parents.get(&current) {
                Some(&parent) if parent > 1 => current = parent,
                _ => return false,
            }
        }
        false
    }
}
