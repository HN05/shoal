//! Gates kept per key, such as one per repository or workspace.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

/// One gate per key. An entry lives while someone holds or awaits its gate,
/// so every concurrent caller for a key shares one gate, even one for a key
/// whose subject is gone.
pub(super) struct Keyed<L>(Mutex<HashMap<String, Weak<L>>>);

impl<L> Default for Keyed<L> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<L: Default> Keyed<L> {
    /// The gate of `key`.
    pub(super) fn gate(&self, key: &str) -> Arc<L> {
        let mut gates = self.live();
        share(&mut gates, key.to_owned())
    }

    /// The gates of `keys` in key order, each once.
    pub(super) fn gates<S: AsRef<str>>(&self, keys: impl IntoIterator<Item = S>) -> Vec<Arc<L>> {
        let mut keys: Vec<String> = keys
            .into_iter()
            .map(|key| key.as_ref().to_owned())
            .collect();
        keys.sort();
        keys.dedup();
        let mut gates = self.live();
        keys.into_iter().map(|key| share(&mut gates, key)).collect()
    }

    /// The map, without the entries of gates nobody holds or awaits.
    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, Weak<L>>> {
        let mut gates = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates.retain(|_, gate| gate.strong_count() > 0);
        gates
    }

    /// How many gates someone holds or awaits.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.live().len()
    }
}

/// The live gate of `key`, created when it has none.
fn share<L: Default>(gates: &mut HashMap<String, Weak<L>>, key: String) -> Arc<L> {
    let entry = gates.entry(key).or_default();
    entry.upgrade().unwrap_or_else(|| {
        let gate = Arc::default();
        *entry = Arc::downgrade(&gate);
        gate
    })
}
