use super::*;
use crate::updates::UpdateKind;
use serde_json::json;

fn github(checks: serde_json::Value) -> serde_json::Value {
    json!({"number": 7, "headRefName": "topic", "headRefOid": "abc", "state": "OPEN",
            "comments": [], "reviews": [], "statusCheckRollup": checks, "mergeable": "MERGEABLE"})
}

#[test]
fn github_status_reruns_on_the_same_head_wake_again() {
    let mut pull = github(json!([
        {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS",
         "targetUrl": "deploy", "startedAt": "first"}
    ]));
    let before = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    pull["statusCheckRollup"][0]["startedAt"] = json!("rerun");
    let next = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    let updates = before.changes(&next, "pr");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
}

#[test]
fn github_comment_reactions_do_not_wake_but_edits_and_review_state_do() {
    let mut pull = github(json!([]));
    pull["comments"] = json!([{"id": "comment", "body": "hello", "reactionGroups": []}]);
    pull["reviews"] = json!([{"id": "review", "body": "", "state": "COMMENTED"}]);
    let before = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    pull["comments"][0]["reactionGroups"] = json!([{"content": "THUMBS_UP"}]);
    let reacted = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    assert!(before.changes(&reacted, "pr").is_empty());
    pull["comments"][0]["body"] = json!("edited");
    let edited = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    assert_eq!(reacted.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
    pull["reviews"][0]["state"] = json!("APPROVED");
    let approved = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    assert_eq!(edited.changes(&approved, "pr")[0].kind, UpdateKind::Comment);
    let mut inline = json!({"id": 1, "body": "finding", "reactions": {}, "updated_at": "first"});
    let original = comment_fingerprint(&inline);
    inline["reactions"] = json!({"+1": 1});
    inline["updated_at"] = json!("reacted");
    assert_eq!(original, comment_fingerprint(&inline));
    inline["body"] = json!("edited finding");
    assert_ne!(original, comment_fingerprint(&inline));
}

#[test]
fn completed_checks_wake_individually_while_other_checks_run() {
    let mut pull = github(json!([
        {"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "rust/1"},
        {"__typename": "CheckRun", "name": "review", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "review/1"},
        {"__typename": "StatusContext", "context": "deploy", "state": "EXPECTED"}
    ]));
    pull["comments"] = json!([{"id": "IC_1", "body": "hello"}]);
    pull["reviews"] = json!([{"id": "PRR_1", "body": "finding"}]);
    pull["mergeable"] = json!("CONFLICTING");
    let mut observed = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    let before = Snapshot {
        conflict: observed.conflict,
        conflict_at: observed.conflict_at.clone(),
        ..Default::default()
    };
    observed.schedule_conflict_reports(&before);
    let updates = before.changes(&observed, "pr");
    assert_eq!(
        updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
        [
            UpdateKind::Comment,
            UpdateKind::CiCompleted,
            UpdateKind::MergeConflict
        ]
    );
    assert_eq!(updates[1].message, "review: SUCCESS");
    assert!(observed.changes(&observed, "pr").is_empty());

    pull["statusCheckRollup"][0]["status"] = json!("COMPLETED");
    pull["statusCheckRollup"][0]["conclusion"] = json!("FAILURE");
    let next = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    let updates = observed.changes(&next, "pr");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].message, "rust: FAILURE");

    pull["headRefOid"] = json!("def");
    let next_head = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    assert_eq!(
        next.changes(&next_head, "pr")
            .iter()
            .filter(|u| u.kind == UpdateKind::CiCompleted)
            .count(),
        2
    );
    pull["state"] = json!("CLOSED");
    let closed = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
    assert_eq!(next_head.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
    assert!(snapshot(&pull.to_string(), 8, Some("topic")).is_err());
    assert!(snapshot(&pull.to_string(), 7, Some("another")).is_err());
    assert!(snapshot(&pull.to_string(), 7, None).is_ok());
    assert!(snapshot(&pull.to_string(), 8, None).is_err());
}

#[test]
fn github_pr_status_reports_state_checks_and_review() {
    let mut pull = github(json!([
        {"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "rust/1"},
        {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS", "targetUrl": "deploy"}
    ]));
    pull["mergeable"] = json!("CONFLICTING");
    pull["reviews"] = json!([{"id": "r", "author": {"login": "a"}, "state": "APPROVED"}]);
    let status = reported_status(&pull.to_string(), 7, "url").unwrap();
    assert_eq!(status.state, Some(PrState::Open));
    assert_eq!(status.merge_conflicts, Some(true));
    assert_eq!(
        status
            .checks
            .iter()
            .map(|check| (check.name.as_str(), check.result.as_str()))
            .collect::<Vec<_>>(),
        [("deploy", "success"), ("rust", "in_progress")]
    );
    assert_eq!(
        status.review,
        Some(crate::review::status::ReviewState::Approved)
    );
    assert!(status.errors.is_empty());

    pull["reviews"][0]["state"] = json!("LATER");
    let status = reported_status(&pull.to_string(), 7, "url").unwrap();
    assert_eq!(status.review, None);
    assert_eq!(status.checks.len(), 2);
    assert!(status.errors[0].starts_with("reviews lookup failed"));
    assert!(reported_status(&pull.to_string(), 8, "url").is_err());
}
