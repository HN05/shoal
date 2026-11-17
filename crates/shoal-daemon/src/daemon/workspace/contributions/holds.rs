//! The holds that keep a contribution from cleanup while `shoal contribute`
//! runs, kept in memory for as long as the connection that placed them.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;

use super::super::Manager;

type Counts = Arc<Mutex<HashMap<RepositoryId, usize>>>;

/// The open holds on each contribution.
#[derive(Default)]
pub(crate) struct ContributionHolds(Counts);

/// Keeps a contribution from cleanup while it lives.
pub(crate) struct ContributionHold {
    counts: Counts,
    repository: RepositoryId,
}

impl ContributionHolds {
    /// Hold the contribution `id`. The caller holds the registry gate, which
    /// the sweep's final check takes too.
    pub(crate) fn hold(&self, id: &RepositoryId) -> ContributionHold {
        *self.0.lock().unwrap().entry(id.clone()).or_default() += 1;
        ContributionHold {
            counts: self.0.clone(),
            repository: id.clone(),
        }
    }

    pub(super) fn held(&self, id: &RepositoryId) -> bool {
        self.0.lock().unwrap().contains_key(id)
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
