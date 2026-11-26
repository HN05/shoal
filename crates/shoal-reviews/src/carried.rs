//! The review a workspace's package carries to the machine it goes to, so
//! a review a sleep, shutdown or move cut off runs where the workspace
//! continues.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    Reviews,
    daemon::Daemon,
    reviews::{Author, CarriedReview, Review, ReviewState},
    store,
};

impl Reviews {
    /// The workspace `id`'s queued or running review, as a package of it
    /// carries it.
    pub async fn pending(&self, id: &SpaceId) -> Result<Option<CarriedReview>> {
        let (workspace, records) = (id.clone(), self.records.clone());
        let review = self
            .store
            .read(move |db| match store::active(db, &workspace)? {
                Some((id, _)) => store::load(db, &*records, id),
                None => Ok(None),
            })
            .await?;
        let active = (review)
            .filter(|review| matches!(review.state, ReviewState::Queued | ReviewState::Running));
        Ok(active.and_then(carried))
    }

    /// Queue `review`, which a package of the workspace `id` carried, again
    /// here, as `shoal review run` would.
    pub async fn queue_carried(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        review: CarriedReview,
    ) -> Result<()> {
        let CarriedReview {
            target,
            agent,
            model,
        } = review;
        let selector = SpaceSelector::id(id);
        (self.queue(daemon, &selector, target, Some(agent), model)).await?;
        Ok(())
    }
}

/// `review` as a package carries it: Shoal's, with the agent and model its
/// reviewer runs, which only Shoal's reviews queue and run with.
fn carried(review: Review) -> Option<CarriedReview> {
    match review.author {
        Author::Shoal { agent, model } => Some(CarriedReview {
            target: review.target,
            agent,
            model,
        }),
        Author::User | Author::Space { .. } => None,
    }
}
