use super::*;
use crate::{reviews::ReviewState, stand_in::Fixture};

async fn fixture() -> Fixture {
    Fixture::new(tempfile::tempdir().unwrap(), "").await
}

impl Fixture {
    /// Start `writer`'s pending review of the workspace.
    async fn start(&self, writer: Writer) -> anyhow::Result<Review> {
        (self.reviews)
            .start_written(&*self.daemon, &self.selector(), writer)
            .await
    }
}

#[tokio::test]
async fn a_writer_keeps_one_pending_review_of_a_workspace() {
    let fixture = fixture().await;
    let review = fixture.start(Writer::User).await.unwrap();
    assert_eq!(review.state, ReviewState::Pending);
    assert_eq!(fixture.start(Writer::User).await.unwrap().id, review.id);
    let parent = Writer::Space("parent".into());
    assert_ne!(fixture.start(parent.clone()).await.unwrap().id, review.id);
    let pending = fixture
        .reviews
        .pending_reviews(&*fixture.daemon, Writer::User);
    let pending: Vec<ReviewId> = pending
        .await
        .unwrap()
        .iter()
        .map(|review| review.id)
        .collect();
    assert_eq!(pending, [review.id]);
}

#[tokio::test]
async fn a_space_reviews_spaces_below_it_and_not_its_own_work() {
    let fixture = fixture().await;
    let own = Writer::Space(fixture.workspace.id.clone());
    let refused = fixture.start(own).await.unwrap_err().to_string();
    assert!(refused.contains("below it"), "{refused}");
}

#[tokio::test]
async fn cancelling_a_pending_review_drops_its_threads_and_only_its_writer_cancels_it() {
    let fixture = fixture().await;
    let parent = Writer::Space("parent".into());
    let review = fixture.start(parent).await.unwrap();
    fixture.records.comment(review.id, "Unclear name");
    let workspace = fixture.selector();
    let by_child = (fixture.reviews).cancel(
        &*fixture.daemon,
        Some(&workspace),
        Some(review.id),
        Some(fixture.workspace.id.clone()),
    );
    let refused = by_child.await.unwrap_err().to_string();
    assert!(refused.contains("cancels it"), "{refused}");
    let by_parent = (fixture.reviews).cancel(
        &*fixture.daemon,
        Some(&workspace),
        None,
        Some("parent".into()),
    );
    let cancelled = by_parent.await.unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    assert!(fixture.records.threads.lock().unwrap().is_empty());
}
