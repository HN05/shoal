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

struct Evidence {
    told: Told,
    /// When the inventory that told it began, before it read anything.
    observed: Instant,
}

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Told> {
        let owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.get(&unique).map(|evidence| evidence.told.clone())
    }

    /// Record what an inventory that began at `observed` told, unless an
    /// inventory that began later told otherwise first, and forget what no
    /// process in `live` names as itself or its parent any more, which no
    /// process can name again, unless a later inventory told it.
    pub(super) fn update(&self, observed: Instant, told: Vec<(u64, Told)>, live: &HashSet<u64>) {
        let mut owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.retain(|unique, evidence| evidence.observed >= observed || live.contains(unique));
        for (unique, told) in told {
            if owners
                .get(&unique)
                .is_none_or(|newest| newest.observed <= observed)
            {
                owners.insert(unique, Evidence { told, observed });
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}
