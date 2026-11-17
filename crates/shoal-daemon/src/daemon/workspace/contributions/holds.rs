//! The holds that keep a contribution from cleanup while `shoal contribute`
//! runs, kept in memory for as long as the connection that placed them.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use tokio::time::Instant;

use super::super::Manager;

/// How long after the daemon starts every contribution counts as held, so
/// the clients whose holds a restart closed hold them again first.
pub(crate) const RESUME_WINDOW: Duration = Duration::from_secs(60);

type Counts = Arc<Mutex<HashMap<RepositoryId, usize>>>;

/// The open holds on each contribution.
#[derive(Default)]
pub(crate) struct ContributionHolds {
    counts: Counts,
    /// Until when every contribution counts as held.
    resumes_until: Mutex<Option<Instant>>,
}

/// Keeps a contribution from cleanup while it lives.
pub(crate) struct ContributionHold {
    counts: Counts,
    repository: RepositoryId,
}

impl ContributionHolds {
    /// Hold the contribution `id`. The caller holds the registry gate, which
    /// the sweep's final check takes too.
    pub(crate) fn hold(&self, id: &RepositoryId) -> ContributionHold {
        *self.counts.lock().unwrap().entry(id.clone()).or_default() += 1;
        ContributionHold {
            counts: self.counts.clone(),
            repository: id.clone(),
        }
    }

    /// Count every contribution as held until `until`, while the holds a
    /// restart closed are resumed.
    pub(crate) fn await_resumes(&self, until: Instant) {
        *self.resumes_until.lock().unwrap() = Some(until);
    }

    pub(super) fn held(&self, id: &RepositoryId) -> bool {
        let resuming = self.resumes_until.lock().unwrap();
        resuming.is_some_and(|until| Instant::now() < until)
            || self.counts.lock().unwrap().contains_key(id)
    }
}

impl Drop for ContributionHold {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap();
        if let Some(count) = counts.get_mut(&self.repository) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.repository);
            }
        }
    }
}

impl Manager {
    /// Hold the registered repository `id` again, as its registration did
    /// before the daemon restarted.
    pub(crate) async fn hold_contribution(&self, id: &RepositoryId) -> Result<ContributionHold> {
        let _registry = self.gates.lock_registry().await;
        let repo = self.repository(id).await?;
        Ok(self.contribution_holds.hold(&repo.id))
    }
}
