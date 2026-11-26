//! The triggers `[triggers.NAME]` configures: their runtime state and their
//! firings.
use std::collections::HashMap;

use shoal_ids::repository_ids::RepositoryId;
use tokio::sync::{Mutex, Notify};

use crate::Runner;

mod commands;
mod configured;
mod firing;
mod state;
mod timer;
mod webhooks;

pub use configured::Configured;
pub use timer::run;
pub use webhooks::run as serve_webhooks;

impl Runner {
    /// Have the triggers' loop read the triggers and their state again
    /// now, as after a change of the config.
    pub fn wake_triggers(&self) {
        self.triggers.wake();
    }
}

/// What the triggers' background loop shares with requests.
#[derive(Default)]
pub(crate) struct Triggers {
    /// Wakes the loop for a changed config or runtime state.
    wake: Notify,
    /// Held through each firing and each pass of the loop, so a trigger
    /// never fires twice for one time.
    firing: Mutex<()>,
    /// Why each repository's triggers could not be read, last time.
    unreadable: std::sync::Mutex<HashMap<RepositoryId, String>>,
}

impl Triggers {
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Record how reading `repository`'s triggers went, returning whether
    /// it failed otherwise than last time, so the log names each failure
    /// once.
    fn read(&self, repository: &RepositoryId, error: Option<String>) -> bool {
        let mut unreadable =
            (self.unreadable.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
        match error {
            Some(error) => unreadable.insert(repository.clone(), error.clone()) != Some(error),
            None => {
                unreadable.remove(repository);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
