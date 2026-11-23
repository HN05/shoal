//! The repository's false positives as the resolutions of its findings'
//! threads keep them: a thread resolved as one is kept there, and drops out
//! once it is reopened or resolved otherwise.
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::threads::{Outcome, Sender, Thread, ThreadKind};
use tokio::sync::MutexGuard;

use crate::{
    Reviews,
    daemon::Daemon,
    false_positives::{self, FalsePositive},
};

/// A repository's false positives as a thread's resolution changed them,
/// before the resolution is recorded. It holds the registry gate, which
/// moves and removal take, so concurrent resolutions change the file and
/// their threads in one order, until it is dropped once the resolution is
/// recorded, or [`Self::undo`] puts the file back when it could not be.
pub struct KeptFalsePositive<'a> {
    _registry: MutexGuard<'a, ()>,
    previous: Option<false_positives::Previous>,
}

impl KeptFalsePositive<'_> {
    /// Put the file back as it was, for a resolution that was not recorded.
    pub async fn undo(self) -> Result<()> {
        match self.previous {
            Some(previous) => previous.restore().await,
            None => Ok(()),
        }
    }
}

impl Reviews {
    /// Keep the finding's thread `thread` in its repository's false
    /// positives while `outcome` is one, or drop it from them. The file
    /// changes first, so an interrupted resolution leaves it with the newer
    /// intent. None for a thread that is no finding of Shoal's reviewer, or
    /// whose workspace is gone.
    pub async fn keep_false_positive(
        &self,
        daemon: &dyn Daemon,
        thread: &Thread,
        outcome: Option<&Outcome>,
    ) -> Result<Option<KeptFalsePositive<'_>>> {
        if thread.kind != ThreadKind::Review || thread.opener != Sender::Shoal {
            return Ok(None);
        }
        let Ok(workspace) = daemon.workspace_by_id(&thread.space_id).await else {
            return Ok(None);
        };
        let finding = self.finding_of(thread).await?;
        let registry = self.gates.lock_registry().await;
        let entry = false_positives::kept(&finding, thread, outcome);
        let repository_id = workspace.repository();
        let path = match entry {
            Some(_) => Some(daemon.writable_false_positives_path(repository_id).await?),
            None => daemon.false_positives_path(repository_id).await?,
        };
        let previous = match path {
            Some(path) => false_positives::keep(path, finding, entry).await?,
            None => None,
        };
        Ok(Some(KeptFalsePositive {
            _registry: registry,
            previous,
        }))
    }

    /// The finding `thread` is, as false positives name it: its review and
    /// its place among that review's findings, such as `17.2`.
    async fn finding_of(&self, thread: &Thread) -> Result<String> {
        let review = (thread.finding.and_then(|finding| finding.review))
            .with_context(|| format!("thread {} names no review", thread.number))?;
        let records = self.records.clone();
        let threads = (self.store)
            .run(move |db| records.review_threads(db, review))
            .await?;
        let place = (threads.iter().position(|found| found.id == thread.id))
            .with_context(|| format!("review {review} did not open thread {}", thread.number))?;
        Ok(format!("{review}.{}", place + 1))
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
