use super::*;
use crate::reviews::{FailureReason, Finding, FindingId, ReviewTarget, Severity};
use crate::time::UnixTime;

fn review(state: ReviewState, findings: usize) -> Review {
    Review {
        id: 17,
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Workspace,
        agent: "claude".into(),
        model: Default::default(),
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
    assert!(message.contains("1 blocker, 1 should-fix"), "{message}");
    assert!(message.contains("Blocked on one bug."), "{message}");
    assert!(message.contains("17.1 blocker at src/a.rs:7"), "{message}");
    assert!(message.ends_with(AUTOMATED_NOTE));
    let clean = agent_message(&review(ReviewState::Finished, 0));
    assert!(clean.contains("Blocked on one bug."), "{clean}");
    assert!(!clean.ends_with(AUTOMATED_NOTE), "{clean}");
}

#[test]
fn a_long_review_fits_one_agent_message() {
    let message = agent_message(&review(ReviewState::Finished, 40));
    assert!(message.len() <= MAX_AGENT_MESSAGE_LEN, "{}", message.len());
    assert!(!message.contains("17.40 "), "{message}");
    assert!(message.ends_with(AUTOMATED_NOTE));
}

#[test]
fn failed_and_cancelled_reviews_say_which() {
    let failed = agent_message(&review(ReviewState::Failed, 0));
    assert!(failed.contains("failed"), "{failed}");
    let cancelled = agent_message(&review(ReviewState::Cancelled, 0));
    assert!(cancelled.contains("cancelled"), "{cancelled}");
}
