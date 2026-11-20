//! Triggers, the daemon's side of `[triggers.NAME]`: it reads the
//! configured triggers, keeps each one's runtime state, and fires them, the
//! timer source's from a background loop, the webhook source's as requests
//! reach its listener, and any trigger at once for `shoal trigger run`.
//! Every firing takes the same steps whatever its source, and launches
//! through the operation `shoal start` uses.
use std::collections::HashMap;

use shoal_ids::repository_ids::RepositoryId;
use tokio::sync::{Mutex, Notify};

mod commands;
mod configured;
mod firing;
mod timer;
mod webhooks;

pub(crate) use configured::Configured;
pub(crate) use timer::run;
pub(crate) use webhooks::run as serve_webhooks;

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
    /// Have the loop read the triggers and their state again now.
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
