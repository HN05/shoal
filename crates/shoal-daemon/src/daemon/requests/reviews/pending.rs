//! The pending reviews a caller writes, and the request that acts on the
//! caller's only one without naming its workspace.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{reviews::Review, space_names::SpaceSelector};
use shoal_reviews::written::Writer;

use crate::daemon::{manager::Manager, scope::Caller};

/// The pending reviews of a request's caller, the user without one.
pub(in crate::daemon) struct Pending<'a> {
    manager: &'a Manager,
    caller: Option<&'a Caller>,
}

impl<'a> Pending<'a> {
    pub(in crate::daemon) fn new(manager: &'a Manager, caller: Option<&'a Caller>) -> Self {
        Self { manager, caller }
    }

    /// The caller's pending reviews, oldest first.
    pub(in crate::daemon) async fn reviews(&self) -> Result<Vec<Review>> {
        (self.manager.reviews)
            .pending_reviews(self.manager, self.writer())
            .await
    }

    /// Submit the caller's only pending review with `summary`, approving
    /// its work when `approve`.
    pub(in crate::daemon) async fn submit_only(
        &self,
        approve: bool,
        summary: Option<String>,
    ) -> Result<Review> {
        let pending = self.reviews().await?;
        ensure!(
            !pending.is_empty(),
            "you have no pending review; start one with shoal review start <space>"
        );
        let only = only(pending)
            .context("you have several pending reviews; name the space whose review to submit")?;
        let workspace = SpaceSelector::id(&only.workspace_id);
        let submitted = (self.manager.reviews).submit(
            self.manager,
            &workspace,
            self.writer(),
            approve,
            summary,
        );
        submitted.await
    }

    /// Cancel, for a command run in the workspace `here` names, the
    /// caller's pending review of it, else its queued or running one, else
    /// the caller's only pending review.
    pub(in crate::daemon) async fn cancel_here(&self, here: &SpaceSelector) -> Result<Review> {
        let reviews = &self.manager.reviews;
        if let Some(id) = reviews
            .to_cancel(self.manager, here, &self.writer())
            .await?
        {
            return reviews
                .cancel(self.manager, None, Some(id), self.by())
                .await;
        }
        let only = only(self.reviews().await?)
            .context("nothing here to cancel, nor is any one review of yours pending")?;
        let cancelled = reviews.cancel(self.manager, None, Some(only.id), self.by());
        cancelled.await
    }

    /// Who writes the caller's reviews.
    fn writer(&self) -> Writer {
        Writer::of(self.caller.map(|caller| &caller.space_id))
    }

    /// The space whose process asks, none for the user.
    fn by(&self) -> Option<SpaceId> {
        self.caller.map(|caller| caller.space_id.clone())
    }
}

/// The only one of `pending`; none for none or several.
fn only(mut pending: Vec<Review>) -> Option<Review> {
    match pending.len() {
        1 => pending.pop(),
        _ => None,
    }
}
