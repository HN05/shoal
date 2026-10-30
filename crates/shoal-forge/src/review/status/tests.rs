use super::*;

#[test]
fn checks_that_ran_and_failed_are_failed() {
    let failed = |result: &str| {
        CheckResult {
            name: "rust".into(),
            result: result.into(),
        }
        .failed()
    };
    for result in ["failure", "error", "timed_out", "startup_failure"] {
        assert!(failed(result), "{result}");
    }
    for result in ["success", "cancelled", "in_progress", "skipped"] {
        assert!(!failed(result), "{result}");
    }
}

#[test]
fn rate_limited_checks_are_reported_as_unread() {
    let snapshot = Snapshot {
        limited: Some(crate::transport::limits::RateLimited {
            host: "forge.example".into(),
            until: u64::MAX,
        }),
        ..Snapshot::default()
    };
    let mut status = PrStatus::new("pr".into(), Some(PrState::Open));
    status.record_snapshot(snapshot);
    assert!(status.checks.is_empty());
    assert_eq!(status.errors.len(), 1, "{:?}", status.errors);
    assert!(status.errors[0].starts_with("CI lookup failed: forge.example limits"));
}

fn review(reviewer: &str, verdict: Verdict) -> (String, Verdict) {
    (reviewer.into(), verdict)
}

#[test]
fn latest_verdict_per_reviewer_decides_and_comments_do_not_withdraw_it() {
    use Verdict::*;
    assert_eq!(summarize([]), ReviewState::Unreviewed);
    assert_eq!(
        summarize([review("bot", Commented)]),
        ReviewState::Commented
    );
    assert_eq!(
        summarize([review("a", Approved), review("a", Commented)]),
        ReviewState::Approved
    );
    assert_eq!(
        summarize([review("a", ChangesRequested), review("a", Approved)]),
        ReviewState::Approved
    );
    assert_eq!(
        summarize([review("a", Approved), review("b", ChangesRequested)]),
        ReviewState::ChangesRequested
    );
}

#[test]
fn forgejo_reviews_skip_pending_stale_and_dismissed_ones() {
    let reviews = serde_json::json!([
        {"user": {"login": "bot"}, "state": "COMMENT"},
        {"user": {"login": "reviewer"}, "state": "REQUEST_CHANGES"},
        {"user": {"login": "old"}, "state": "APPROVED", "stale": true},
        {"user": {"login": "gone"}, "state": "APPROVED", "dismissed": true},
        {"user": {"login": "me"}, "state": "PENDING"},
        {"user": {"login": "asked"}, "state": "REQUEST_REVIEW"}
    ]);
    assert_eq!(
        forgejo_reviews(reviews.as_array().unwrap()).unwrap(),
        [
            review("bot", Verdict::Commented),
            review("reviewer", Verdict::ChangesRequested)
        ]
    );
    assert!(forgejo_reviews(&[serde_json::json!({"state": "LATER"})]).is_err());
}

#[test]
fn github_reviews_skip_drafts_and_dismissals() {
    let reviews = [
        serde_json::json!({"user": {"login": "a"}, "state": "APPROVED"}),
        serde_json::json!({"user": {"login": "b"}, "state": "DISMISSED"}),
        serde_json::json!({"user": {"login": "c"}, "state": "PENDING"}),
        serde_json::json!({"user": {"login": "d"}, "state": "COMMENTED"}),
    ];
    assert_eq!(
        github_reviews(&reviews).unwrap(),
        [
            review("a", Verdict::Approved),
            review("d", Verdict::Commented)
        ]
    );
    assert!(github_reviews(&[serde_json::json!({"state": "LATER"})]).is_err());
}
