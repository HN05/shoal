//! The workspace agent's answers to a review's findings: replies, and
//! outcomes that resolve or reopen them, kept among the repository's false
//! positives while one is resolved as one.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::time::UnixTime;

use crate::{
    Reviews,
    daemon::Daemon,
    false_positives::{self, FalsePositive},
    reviews::{FindingId, Outcome, Review},
    store, validate,
};

impl Reviews {
    /// Add a reply to `finding`.
    pub async fn reply(
        &self,
        daemon: &dyn Daemon,
        finding: FindingId,
        body: String,
    ) -> Result<Review> {
        validate::agent_message(&body)?;
        let now = UnixTime::now();
        self.store
            .run(move |db| store::reply(db, finding, &body, now))
            .await?;
        self.review(daemon, finding.review).await
    }

    /// Resolve `finding` with `outcome`, or reopen it, and keep it among its
    /// repository's false positives only while it is resolved as one. The
    /// file changes first and goes back when the outcome cannot be stored,
    /// so an interrupted resolve leaves the file with the newer intent.
    pub async fn resolve(
        &self,
        daemon: &dyn Daemon,
        finding: FindingId,
        outcome: Option<Outcome>,
    ) -> Result<Review> {
        // Moves and removal take this gate too. Holding it over both steps
        // keeps concurrent resolves in one order in the outcome and the file.
        let registry = self.gates.lock_registry().await;
        let resolved = self.stored(finding.review).await?;
        let previous = (keep_false_positive(daemon, &resolved, finding, outcome.as_ref())).await?;
        if let Err(error) = self.record_outcome(finding, outcome).await {
            if let Some(previous) = previous {
                previous.restore().await?;
            }
            return Err(error);
        }
        drop(registry);
        self.review(daemon, finding.review).await
    }

    /// Store `outcome` as `finding`'s, or reopen it without one.
    async fn record_outcome(&self, finding: FindingId, outcome: Option<Outcome>) -> Result<()> {
        self.store
            .run(move |db| store::resolve(db, finding, outcome.as_ref()))
            .await
    }
}

/// The false positives of the repository `repository_id`, newest first. An
/// error reading them has the context [`false_positives::Unreadable`].
pub(crate) async fn listed(
    daemon: &dyn Daemon,
    repository_id: &RepositoryId,
) -> Result<Vec<FalsePositive>> {
    false_positives::listed(daemon.false_positives_path(repository_id).await?).await
}

/// Keep `finding` of `review` in its repository's false positives while
/// `outcome` is one, or drop it from them, and return the file as it was
/// when that changed it. The caller holds the registry gate, which moves and
/// removal take, so the file stays where it was found.
async fn keep_false_positive(
    daemon: &dyn Daemon,
    review: &Review,
    finding: FindingId,
    outcome: Option<&Outcome>,
) -> Result<Option<false_positives::Previous>> {
    let entry = false_positives::kept(review, finding, outcome)?;
    let workspace = daemon.workspace_by_id(&review.workspace_id).await?;
    let repository_id = workspace.repository();
    let path = match entry {
        Some(_) => Some(daemon.writable_false_positives_path(repository_id).await?),
        None => daemon.false_positives_path(repository_id).await?,
    };
    let Some(path) = path else {
        return Ok(None);
    };
    false_positives::keep(path, finding, entry).await
}
