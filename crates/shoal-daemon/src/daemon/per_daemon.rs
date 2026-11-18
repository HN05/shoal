//! What fake workers record, and what tests tell them, kept apart by the
//! state directory of the daemon they serve: a test binary that runs whole,
//! as `cargo test` does, runs many tests' daemons in one process, and each
//! test reads only its own.
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

use crate::paths::Paths;

pub struct PerDaemon<T>(Mutex<BTreeMap<PathBuf, Vec<T>>>);

impl<T> PerDaemon<T> {
    pub const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// Add `value` to those of the daemon of `paths`.
    pub fn push(&self, paths: &Paths, value: T) {
        let mut values = self.0.lock().unwrap();
        values.entry(paths.state.clone()).or_default().push(value);
    }
}

impl<T: Clone> PerDaemon<T> {
    /// The values of the daemon of `paths`, oldest first.
    pub fn of(&self, paths: &Paths) -> Vec<T> {
        let values = self.0.lock().unwrap();
        values.get(&paths.state).cloned().unwrap_or_default()
    }
}

impl<T: PartialEq> PerDaemon<T> {
    /// Whether the daemon of `paths` has `value`.
    pub fn contains(&self, paths: &Paths, value: &T) -> bool {
        let values = self.0.lock().unwrap();
        (values.get(&paths.state)).is_some_and(|values| values.contains(value))
    }
}
