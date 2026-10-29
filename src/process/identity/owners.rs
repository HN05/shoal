//! Who owned the processes inventories attributed, by unique process ID, so
//! a process stays attributable once the ancestors that told have exited. A
//! process keeps its own unique ID for life, and one launchd adopted keeps its
//! exited parent's until it replaces its image; neither is ever reused within
//! a boot, which the daemon's memory never outlives.
use super::Marks;
use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex, PoisonError},
};

/// The daemon's memory, which every inventory consults and updates.
pub(super) static OWNERS: LazyLock<Owners> = LazyLock::new(Owners::default);

/// Inventories that update the memory take turns, so each one lists and
/// reads processes after every reading recorded before it.
pub(super) static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What an inventory told about a process's owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Told {
    Owner(Marks),
    /// Its readable environment, or its nearest ancestor's, carries no marker.
    NoExecution,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Evidence {
    pub(super) told: Told,
    /// Read from the process's own environment, rather than inferred from an
    /// ancestor's, which a process that replaced its image may no longer
    /// share.
    pub(super) direct: bool,
}

#[derive(Default)]
pub(super) struct Owners(Mutex<Memory>);

#[derive(Default)]
struct Memory {
    entries: HashMap<u64, Entry>,
    /// How many inventories have updated the memory.
    inventories: u64,
}

struct Entry {
    evidence: Evidence,
    /// The inventory that last named the process.
    named: u64,
}

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Evidence> {
        let memory = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memory
            .entries
            .get(&unique)
            .map(|entry| entry.evidence.clone())
    }

    /// Record an inventory's evidence over what earlier ones told, except an
    /// inference over a reading of the process itself, and forget what
    /// neither it nor the inventory before it named, as no process in `live`
    /// names it as itself or its parent: a process that forked and exited
    /// between one inventory's listing and its reading leaves a child only the
    /// next inventory lists, and after that no process can name it again.
    pub(super) fn update(&self, evidence: Vec<(u64, Evidence)>, live: &HashSet<u64>) {
        let mut memory = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memory.inventories += 1;
        let now = memory.inventories;
        for unique in live {
            if let Some(entry) = memory.entries.get_mut(unique) {
                entry.named = now;
            }
        }
        memory.entries.retain(|_, entry| entry.named + 1 >= now);
        for (unique, evidence) in evidence {
            match memory.entries.get_mut(&unique) {
                Some(entry) if !evidence.direct && entry.evidence.direct => entry.named = now,
                Some(entry) => {
                    *entry = Entry {
                        evidence,
                        named: now,
                    }
                }
                None => {
                    memory.entries.insert(
                        unique,
                        Entry {
                            evidence,
                            named: now,
                        },
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        let memory = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memory.entries.len()
    }
}
