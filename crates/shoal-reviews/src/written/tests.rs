use super::*;
use crate::{required::Gate, reviews::ReviewState, stand_in::Fixture};
use shoal_protocol::threads::Outcome;

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

impl Fixture {
    /// Submit `writer`'s only pending review, approving when `approve`.
    async fn submit(&self, writer: Writer, approve: bool) -> anyhow::Result<Review> {
        let summary = Some("Close, but rename it.".to_owned());
        (self.reviews)
            .submit(&*self.daemon, None, writer, approve, summary)
            .await
    }

    /// Whether a review gate lets work at the workspace's HEAD through.
    async fn covered(&self) -> bool {
        let head = crate::daemon::Spaces::head(&*self.daemon, &self.workspace);
        let head = head.await.unwrap();
        let checked = self
            .reviews
            .check_reviewed(&self.workspace, Gate::Ready, &head);
        checked.await.is_ok()
    }
}

#[tokio::test]
async fn submitting_delivers_the_comments_together_and_the_gate_waits_for_their_writer() {
    let fixture = fixture().await;
    let review = fixture.start(Writer::User).await.unwrap();
    let comment = fixture.records.comment(review.id, "Unclear name");
    let submitted = fixture.submit(Writer::User, false).await.unwrap();
    assert_eq!(submitted.state, ReviewState::Finished);
    assert_eq!(submitted.verdict, Some(Verdict::RequestChanges));
    assert!(submitted.head.is_some());
    assert!(!submitted.threads[0].held);
    let messages = fixture.daemon.messages.lock().unwrap().clone();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Unclear name"), "{messages:?}");
    assert_eq!(fixture.daemon.notices.lock().unwrap().len(), 1);
    assert!(!fixture.covered().await);
    fixture.records.resolve(comment, Some(Outcome::Fixed));
    assert!(fixture.covered().await);
    // It was submitted; nothing is left to submit.
    assert!(fixture.submit(Writer::User, false).await.is_err());
}

#[tokio::test]
async fn a_review_without_comments_covers_its_commit_only_when_it_approves() {
    let fixture = fixture().await;
    let parent = Writer::Space("parent".into());
    fixture.start(parent.clone()).await.unwrap();
    let commented = fixture.submit(parent.clone(), false).await.unwrap();
    assert_eq!(commented.verdict, Some(Verdict::Comment));
    assert!(!fixture.covered().await);
    fixture.start(parent.clone()).await.unwrap();
    fixture.submit(parent.clone(), true).await.unwrap();
    assert!(fixture.covered().await);
    // A later review that only comments never takes the gate back.
    fixture.start(parent.clone()).await.unwrap();
    fixture.submit(parent, false).await.unwrap();
    assert!(fixture.covered().await);
    // A space above wrote it, so no coordinator is told of it.
    assert!(fixture.daemon.notices.lock().unwrap().is_empty());
}

#[tokio::test]
async fn removing_the_reviewed_workspace_drops_its_pending_reviews_with_their_comments() {
    let fixture = fixture().await;
    let review = fixture.start(Writer::Space("parent".into())).await.unwrap();
    fixture.records.comment(review.id, "Unclear name");
    let (records, workspace) = (fixture.records.clone(), fixture.workspace.id.clone());
    let dropped = fixture.reviews.store.write(move |tx| {
        drop_pending_of_removed(tx, &*records, &workspace)?;
        crate::store::load(tx, &*records, review.id)
    });
    let dropped = dropped.await.unwrap().unwrap();
    assert_eq!(dropped.state, ReviewState::Cancelled);
    assert!(fixture.records.threads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_summary_leaves_its_message_room_for_the_comments() {
    let fixture = fixture().await;
    fixture.start(Writer::User).await.unwrap();
    let long = "a".repeat(MAX_SUMMARY_LEN + 1);
    let submitted =
        (fixture.reviews).submit(&*fixture.daemon, None, Writer::User, true, Some(long));
    assert!(submitted.await.is_err());
}

#[tokio::test]
async fn a_writers_unresolved_comments_keep_the_gate_closed_whatever_reviews_after() {
    let fixture = fixture().await;
    let review = fixture.start(Writer::User).await.unwrap();
    let comment = fixture.records.comment(review.id, "Unclear name");
    fixture.submit(Writer::User, false).await.unwrap();
    // A later review of the same commit that approves leaves the comment
    // to its writer.
    fixture.start(Writer::Space("parent".into())).await.unwrap();
    fixture
        .submit(Writer::Space("parent".into()), true)
        .await
        .unwrap();
    assert!(!fixture.covered().await);
    fixture.records.resolve(comment, Some(Outcome::Fixed));
    assert!(fixture.covered().await);
}

#[tokio::test]
async fn a_workspaces_latest_review_is_never_a_pending_one() {
    let fixture = fixture().await;
    fixture.start(Writer::User).await.unwrap();
    fixture.submit(Writer::User, true).await.unwrap();
    let selector = fixture.selector();
    let submitted = fixture.reviews.latest(&*fixture.daemon, &selector);
    let submitted = submitted.await.unwrap().id;
    fixture.start(Writer::Space("parent".into())).await.unwrap();
    let latest = fixture.reviews.latest(&*fixture.daemon, &selector);
    assert_eq!(latest.await.unwrap().id, submitted);
}
