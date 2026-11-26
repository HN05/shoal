//! Shoal's log of automatic cleanup: each pass that removed spaces, naming
//! them, or that failed in a new way.
use crate::Spaces;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::events::{ShoalEventDetails, ShoalEventKind};
use shoal_protocol::space_names::SpaceName;

/// The spaces removed since the log last recorded a pass.
#[derive(Default)]
pub struct Removed(Mutex<Vec<SpaceName>>);

impl Removed {
    fn lock(&self) -> MutexGuard<'_, Vec<SpaceName>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn push(&self, name: &SpaceName) {
        self.lock().push(name.clone());
    }

    fn take(&self) -> Vec<SpaceName> {
        std::mem::take(&mut *self.lock())
    }
}

impl Spaces {
    /// Records the pass only when spaces were removed since the last one, or
    /// when its failures differ from the pass before.
    pub async fn record_cleanup_run(&self, failed: Option<String>) {
        let removed = self.cleanup_removals.take();
        if removed.is_empty() && failed.is_none() {
            return;
        }
        let mut details = ShoalEventDetails::new(ShoalEventKind::CleanupRan);
        details.removed = removed;
        details.error = failed;
        crate::events::record_shoal(&self.store, details).await;
    }
}

#[cfg(test)]
mod tests;
