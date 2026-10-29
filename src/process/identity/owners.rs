//! Who owned the processes inventories attributed, by unique process ID, so
//! a process stays attributable once the ancestors that told have exited. A
//! process keeps its own unique ID for life, and one launchd adopted keeps its
//! exited parent's until it replaces its image; neither is ever reused within
//! a boot, which the daemon's memory never outlives.
use super::Marks;
use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex, PoisonError},
    time::Instant,
};

/// The daemon's memory, which every inventory consults and updates.
pub(super) static OWNERS: LazyLock<Owners> = LazyLock::new(Owners::default);

/// What an inventory told about a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Told {
    Owner(Marks),
    /// Its readable environment, or its nearest ancestor's, carries no marker.
    NoExecution,
}

#[derive(Default)]
pub(super) struct Owners(Mutex<HashMap<u64, Evidence>>);

/// What an inventory told, and when it began, before it read anything; an
/// answer taken from memory keeps the time it was first told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Evidence {
    pub(super) told: Told,
    pub(super) observed: Instant,
}

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Evidence> {
        let owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.get(&unique).cloned()
    }

    /// Record each piece of evidence unless evidence observed later is
    /// recorded already, and forget what no process in `live` names as
    /// itself or its parent any more, which no process can name again, unless
    /// it was observed after `since`, when the updating inventory began.
    pub(super) fn update(
        &self,
        since: Instant,
        evidence: Vec<(u64, Evidence)>,
        live: &HashSet<u64>,
    ) {
        let mut owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.retain(|unique, recorded| recorded.observed >= since || live.contains(unique));
        for (unique, evidence) in evidence {
            if owners
                .get(&unique)
                .is_none_or(|newest| newest.observed <= evidence.observed)
            {
                owners.insert(unique, evidence);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}
