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
        let parents = (same_user_processes().await?.into_iter())
            .map(|(pid, _, parent)| (pid, parent))
            .collect();
        Ok(Self {
            wrappers: live,
            parents,
        })
    }

    /// The agent wrappers other than `own`, the wrapper of the execution
    /// whose processes are sought.
    pub fn others(&self, own: Option<&Identity>) -> Vec<Identity> {
        (self.wrappers.values())
            .filter(|wrapper| own != Some(*wrapper))
            .cloned()
            .collect()
    }

    /// Whether the process `pid` belongs to another execution's agent than
    /// the one `own` wraps: whether it is, or runs below, an agent wrapper
    /// other than `own` that is nearer to it than `own`. An agent a scoped
    /// caller runs in its own terminal runs below the caller's wrapper too,
    /// and its processes stay its own.
    pub fn foreign(&self, pid: u32, own: Option<&Identity>) -> bool {
        let own = own.filter(|own| alive(own).unwrap_or(false));
        let mut current = pid;
        for _ in 0..=self.parents.len() {
            if own.is_some_and(|own| own.pid == current) {
                return false;
            }
            if self.wrappers.contains_key(&current) {
                return true;
            }
            match self.parents.get(&current) {
                Some(&parent) if parent > 1 => current = parent,
                _ => return false,
            }
        }
        false
    }
}
