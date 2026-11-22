use super::*;
use crate::reviews::{Author, FailureReason, ReviewTarget, Severity};
use crate::stand_in::finding;
use crate::time::UnixTime;

fn review(state: ReviewState, findings: u32) -> Review {
    Review {
        id: ReviewId(17),
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Workspace,
        author: Author::Shoal {
            agent: "claude".into(),
            model: Default::default(),
        },
        reviewer: Some("claude".into()),
        state,
        failure: (state == ReviewState::Failed).then_some(FailureReason::RateLimit),
        head: None,
        base: None,
        created_at: UnixTime::from_seconds(1),
        started_at: None,
        ended_at: None,
        summary: Some("Blocked on one bug.".into()),
        verdict: None,
        threads: (1..=findings)
            .map(|item| {
                let severity = match item {
                    1 => Severity::Blocker,
                    _ => Severity::ShouldFix,
                };
                finding(
                    item,
                    "src/a.rs",
                    severity,
                    &"It panics on an empty list.".repeat(20),
                )
            })
            .collect(),
        reopened: Vec::new(),
        posted: None,
        outdated: None,
    }
}

#[test]
fn a_finished_review_delivers_its_findings_and_the_automated_note() {
    let message = agent_message(&review(ReviewState::Finished, 2));
    assert!(message.contains("1 blocker, 1 should-fix"), "{message}");
    assert!(message.contains("Blocked on one bug."), "{message}");
    assert!(
        message.contains("1.1 blocker at src/a.rs:7: Problem 1"),
        "{message}"
    );
    assert!(message.ends_with(AUTOMATED_NOTE));
    let clean = agent_message(&review(ReviewState::Finished, 0));
    assert!(clean.contains("Blocked on one bug."), "{clean}");
    assert!(!clean.ends_with(AUTOMATED_NOTE), "{clean}");
}

#[test]
fn a_long_review_fits_one_agent_message() {
    let message = agent_message(&review(ReviewState::Finished, 40));
    assert!(message.len() <= MAX_AGENT_MESSAGE_LEN, "{}", message.len());
    assert!(!message.contains("1.40 "), "{message}");
    assert!(message.ends_with(AUTOMATED_NOTE));
}

#[test]
fn a_fix_still_present_is_delivered_with_why() {
    let mut review = review(ReviewState::Finished, 0);
    let mut reopened = finding(3, "src/b.rs", Severity::Nit, "It breaks.");
    let mut post = reopened.posts[0].clone();
    post.post.text = "The empty case still panics.".into();
    reopened.posts.push(post);
    review.reopened.push(reopened);
    let message = agent_message(&review);
    assert!(message.contains("no new findings"), "{message}");
    assert!(
        message.contains("1.3 nit at src/b.rs:7: Problem 3 (reopened"),
        "{message}"
    );
    assert!(
        message.contains("The empty case still panics."),
        "{message}"
    );
}

#[test]
fn failed_and_cancelled_reviews_say_which() {
    let failed = agent_message(&review(ReviewState::Failed, 0));
    assert!(failed.contains("failed"), "{failed}");
    let cancelled = agent_message(&review(ReviewState::Cancelled, 0));
    assert!(cancelled.contains("cancelled"), "{cancelled}");
}
