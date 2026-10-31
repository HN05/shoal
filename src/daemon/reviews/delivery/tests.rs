use super::*;
use crate::reviews::{FailureReason, Finding, FindingId, ReviewTarget, Severity};

fn review(state: ReviewState, findings: usize) -> Review {
    Review {
        id: 17,
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Workspace,
        agent: "claude".into(),
        reviewer: Some("claude".into()),
        state,
        failure: (state == ReviewState::Failed).then_some(FailureReason::RateLimit),
        head: None,
        base: None,
        created_at: 1,
        started_at: None,
        ended_at: None,
        summary: Some("Blocked on one bug.".into()),
        verdict: None,
        findings: (1..=findings as i64)
            .map(|number| Finding {
                id: FindingId { review: 17, number },
                severity: if number == 1 {
                    Severity::Blocker
                } else {
                    Severity::ShouldFix
                },
                path: "src/a.rs".into(),
                line: 7,
                title: "Unchecked index".into(),
                body: "It panics on an empty list.".repeat(20),
                replies: Vec::new(),
                outcome: None,
                check: None,
            })
            .collect(),
        posted: None,
        outdated: None,
    }
}

#[test]
fn a_finished_review_delivers_its_findings_and_the_automated_note() {
    let message = agent_message(&review(ReviewState::Finished, 2));
    assert!(
        message.starts_with("Review 17 finished: 1 blocker, 1 should-fix. Blocked on one bug.")
    );
    assert!(message.contains("17.1 blocker at src/a.rs:7: Unchecked index\nIt panics"));
    assert!(message.contains("shoal review resolve <finding> <outcome>"));
    assert!(message.ends_with(AUTOMATED_NOTE));
    assert_eq!(
        notice(&review(ReviewState::Finished, 2)),
        "review 17 finished: 1 blocker, 1 should-fix"
    );
    let clean = agent_message(&review(ReviewState::Finished, 0));
    assert_eq!(
        clean,
        "Review 17 finished with no findings: Blocked on one bug."
    );
}

#[test]
fn a_long_review_fits_one_agent_message() {
    let message = agent_message(&review(ReviewState::Finished, 40));
    assert!(message.len() <= MAX_AGENT_MESSAGE_LEN, "{}", message.len());
    assert!(message.contains("more findings are not shown"));
    assert!(message.contains("shoal view review 17"));
    assert!(message.ends_with(AUTOMATED_NOTE));
}

#[test]
fn failed_and_cancelled_reviews_say_why_briefly() {
    let failed = agent_message(&review(ReviewState::Failed, 0));
    assert!(
        failed.starts_with("Review 17 failed: the reviewer was rate limited"),
        "{failed}"
    );
    assert_eq!(
        notice(&review(ReviewState::Failed, 0)),
        "review 17 failed: rate-limit"
    );
    let cancelled = agent_message(&review(ReviewState::Cancelled, 0));
    assert!(cancelled.starts_with("Review 17 was cancelled."));
}
