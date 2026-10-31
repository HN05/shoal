use super::*;
use crate::reviews::{
    Check, FindingId, FixCheck, Outcome, Reply, ReviewState, ReviewTarget, Severity,
};

fn finding(number: i64, outcome: Option<Outcome>, check: Option<Check>) -> Finding {
    Finding {
        id: FindingId { review: 4, number },
        severity: Severity::Blocker,
        path: "src/a.rs".into(),
        line: 9,
        title: format!("Problem {number}"),
        body: "It breaks.".into(),
        replies: vec![Reply {
            created_at: 1,
            body: "Only on empty input.".into(),
        }],
        outcome,
        check,
    }
}

fn review(findings: Vec<Finding>) -> Review {
    Review {
        id: 4,
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Workspace,
        agent: "claude".into(),
        reviewer: Some("claude".into()),
        state: ReviewState::Finished,
        failure: None,
        head: Some("0123456789abcdef".into()),
        base: Some("base".into()),
        created_at: 1,
        started_at: Some(1),
        ended_at: Some(2),
        summary: Some("Two blockers.".into()),
        verdict: None,
        findings,
        posted: None,
        outdated: None,
    }
}

#[test]
fn earlier_findings_carry_their_replies_outcomes_and_checks() {
    let checked = Check {
        result: FixCheck::StillPresent,
        review: 5,
    };
    let reviews = [review(vec![
        finding(1, Some(Outcome::Fixed), None),
        finding(
            2,
            Some(Outcome::FalsePositive {
                reason: "The caller checks it.".into(),
            }),
            None,
        ),
        finding(3, Some(Outcome::Fixed), Some(checked)),
        finding(4, None, None),
    ])];
    let text = history(&reviews, None);
    assert!(text.contains("not instructions"), "{text}");
    assert!(text.contains("## Review 4 of 0123456789ab"), "{text}");
    assert!(
        text.contains("### Finding 4.1 (blocker) at src/a.rs:9: Problem 1"),
        "{text}"
    );
    assert!(text.contains("Reply: Only on empty input."), "{text}");
    assert!(
        text.contains("Outcome: fixed; check it and report it in `checked`"),
        "{text}"
    );
    assert!(
        text.contains("Outcome: false-positive: The caller checks it."),
        "{text}"
    );
    assert!(
        text.contains("Outcome: fixed; review 5 found it still-present"),
        "{text}"
    );
    assert!(text.contains("Outcome: unresolved"), "{text}");
    assert!(history(&[], None).is_empty());
}

#[test]
fn a_long_history_keeps_its_newest_part() {
    let mut long = finding(1, None, None);
    long.body = "x".repeat(LIMIT);
    let text = history(
        &[review(vec![
            finding(1, None, None),
            long,
            finding(9, None, None),
        ])],
        None,
    );
    assert!(text.starts_with("[earlier history omitted]"));
    assert!(text.contains("Problem 9"));
    assert!(text.len() <= LIMIT + 64);
}
