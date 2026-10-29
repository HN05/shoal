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
pub(super) struct Owners(Mutex<HashMap<u64, Entry>>);

/// What an inventory told about a process, and when it read the process; an
/// answer taken from memory keeps the time it was first read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Evidence {
    pub(super) told: Told,
    pub(super) observed: Instant,
    /// Read from the process's own environment, rather than inferred from an
    /// ancestor's, which a process that replaced its image may no longer
    /// share.
    pub(super) direct: bool,
}

impl Evidence {
    /// Whether this replaces `recorded`: a reading of the process itself
    /// outranks any inference, and otherwise the later observation wins.
    fn supersedes(&self, recorded: &Evidence) -> bool {
        (self.direct, self.observed) >= (recorded.direct, recorded.observed)
    }
}

struct Entry {
    evidence: Evidence,
    /// When the latest inventory that named the process began.
    named: Instant,
}

impl Owners {
    pub(super) fn recall(&self, unique: u64) -> Option<Evidence> {
        let owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.get(&unique).map(|entry| entry.evidence.clone())
    }

    /// Record the evidence an inventory that began at `since` gathered,
    /// where it supersedes what is recorded, and forget what neither that
    /// inventory nor a later one named: no process in `live` names it as
    /// itself or its parent any more, and no process can name it again.
    pub(super) fn update(
        &self,
        since: Instant,
        evidence: Vec<(u64, Evidence)>,
        live: &HashSet<u64>,
    ) {
        let mut owners = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        owners.retain(|unique, entry| entry.named >= since || live.contains(unique));
        for unique in live {
            if let Some(entry) = owners.get_mut(unique) {
                entry.named = entry.named.max(since);
            }
        }
        for (unique, evidence) in evidence {
            match owners.get_mut(&unique) {
                Some(entry) => {
                    entry.named = entry.named.max(since);
                    if evidence.supersedes(&entry.evidence) {
                        entry.evidence = evidence;
                    }
                }
                None => {
                    owners.insert(
                        unique,
                        Entry {
                            evidence,
                            named: since,
                        },
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}
