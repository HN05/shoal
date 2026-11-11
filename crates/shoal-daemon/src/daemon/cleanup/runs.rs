//! Shoal's log of automatic cleanup: each pass after spaces were removed,
//! naming them, or that failed in a new way.
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{
    client::space_names::SpaceName,
    daemon::{
        events::{ShoalEventDetails, ShoalEventKind},
        workspace::Manager,
    },
};

/// The spaces automatic cleanup removed since the log last recorded a pass.
#[derive(Default)]
pub(crate) struct Removed(Mutex<Vec<SpaceName>>);

impl Removed {
    fn lock(&self) -> MutexGuard<'_, Vec<SpaceName>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Automatic cleanup removed the space `name`.
    pub(crate) fn push(&self, name: &SpaceName) {
        self.lock().push(name.clone());
    }

    fn take(&self) -> Vec<SpaceName> {
        std::mem::take(&mut *self.lock())
    }
}

impl Manager {
    /// Record the pass that just ended in Shoal's log when spaces were
    /// removed since the last, or with `failed`, its failures, when they
    /// differ from the pass before.
    pub(super) async fn record_cleanup_run(&self, failed: Option<String>) {
        let removed = self.cleanup_removals.take();
        if removed.is_empty() && failed.is_none() {
            return;
        }
        let mut details = ShoalEventDetails::new(ShoalEventKind::CleanupRan);
        details.removed = removed;
        details.error = failed;
        self.record_shoal(details).await;
    }
}

#[cfg(test)]
mod tests;
