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

#[derive(Default)]
pub(super) struct Owners(Mutex<HashMap<u64, Owner>>);

struct Owner {
    marks: Marks,
    recorded: Instant,
}

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Marks> {
        let owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.get(&unique).map(|owner| owner.marks.clone())
    }

    /// Remember the owners of `attributed`, and forget those no process in
    /// `live` names as itself or its parent any more, which no process can
    /// name again, unless an inventory that started after `since` recorded
    /// them.
    pub(super) fn update(
        &self,
        since: Instant,
        attributed: Vec<(u64, Marks)>,
        live: &HashSet<u64>,
    ) {
        let recorded = Instant::now();
        let mut owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.retain(|unique, owner| owner.recorded >= since || live.contains(unique));
        for (unique, marks) in attributed {
            owners.insert(unique, Owner { marks, recorded });
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}
