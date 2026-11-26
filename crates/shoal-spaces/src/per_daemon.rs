//! What fake workers record, kept apart by the state directory of the daemon
//! they serve: `cargo test` runs many tests' daemons in one process, and
//! each test reads only its own.
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

use crate::paths::Paths;

pub struct PerDaemon<T>(Mutex<BTreeMap<PathBuf, Vec<T>>>);

impl<T> PerDaemon<T> {
    pub const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    pub fn push(&self, paths: &Paths, value: T) {
        let mut values = self.0.lock().unwrap();
        values.entry(paths.state.clone()).or_default().push(value);
    }
}

#[cfg(any(test, feature = "test-support"))]
impl<T: Clone> PerDaemon<T> {
    /// Oldest first.
    pub fn of(&self, paths: &Paths) -> Vec<T> {
        let values = self.0.lock().unwrap();
        values.get(&paths.state).cloned().unwrap_or_default()
    }
}
