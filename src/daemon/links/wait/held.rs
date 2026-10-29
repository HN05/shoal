//! Passing CI checks held back while other checks on the PR's head are
//! pending, so its agent wakes when a check fails or every check has passed.
use serde::{Deserialize, Serialize};

use crate::forge::{
    review::status::CheckResult,
    updates::{Snapshot, Update},
};

/// The passing checks of a head commit that are not delivered yet.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct HeldPasses {
    head: String,
    updates: Vec<Update>,
}

impl HeldPasses {
    pub(super) fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    /// The updates of `observed` to deliver now that `snapshot` is seen.
    /// Passing checks are held while another check on the head is pending;
    /// a failed check, or the last pending one finishing, delivers the passes
    /// held for that head with it. A new head drops the previous head's.
    pub(super) fn deliver(&mut self, snapshot: &Snapshot, observed: Vec<Update>) -> Vec<Update> {
        if self.head != snapshot.head() {
            snapshot.head().clone_into(&mut self.head);
            self.updates.clear();
        }
        let failed = observed
            .iter()
            .any(|update| update.check.as_ref().is_some_and(|check| !check.passed()));
        if failed || !snapshot.checks_pending() {
            let mut delivered = std::mem::take(&mut self.updates);
            delivered.extend(observed);
            return delivered;
        }
        let (passes, delivered) = observed.into_iter().partition::<Vec<_>, _>(|update| {
            update.check.as_ref().is_some_and(CheckResult::passed)
        });
        self.updates.extend(passes);
        delivered
    }
}

#[cfg(test)]
mod tests;
