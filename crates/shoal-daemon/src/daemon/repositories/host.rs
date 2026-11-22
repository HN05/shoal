//! The host the daemon runs on, whose headroom seed builds wait for.
use std::path::Path;

use shoal_repositories::daemon::Host;

use crate::daemon::{
    capacity::{self, MemoryState},
    manager::Manager,
};

impl Host for Manager {
    fn clear_of_overload(&self, paths: &[&Path]) -> bool {
        let overload = &self.config().machine.overload;
        let sample = capacity::sample(paths, overload, self.work_limit());
        let disk_floor = if overload.warning.enabled {
            overload.warning.disk_free_bytes()
        } else {
            0
        };
        sample.memory == MemoryState::Healthy
            && sample.disk_free.is_some_and(|free| free >= disk_floor)
    }
}
