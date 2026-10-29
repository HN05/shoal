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
pub(super) struct Owners(Mutex<HashMap<u64, Evidence>>);

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Evidence> {
        let owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.get(&unique).cloned()
    }

    /// Record an inventory's evidence over what earlier ones told, except an
    /// inference over a reading of the process itself, and forget what no
    /// process in `live` names as itself or its parent any more, which no
    /// process can name again.
    pub(super) fn update(&self, evidence: Vec<(u64, Evidence)>, live: &HashSet<u64>) {
        let mut owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.retain(|unique, _| live.contains(unique));
        for (unique, evidence) in evidence {
            if owners
                .get(&unique)
                .is_none_or(|recorded| evidence.direct || !recorded.direct)
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
