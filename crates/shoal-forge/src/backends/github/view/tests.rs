use super::*;
use serde_json::json;

#[test]
fn reviewers_are_the_requested_users_and_the_owners_teams() {
    let pull = json!({"number": 7, "title": "Fix", "body": "", "state": "open",
        "user": null, "labels": [], "created_at": "t0", "draft": false,
        "head": {"ref": "topic"}, "base": {"ref": "main"},
        "requested_reviewers": [{"login": "kim"}],
        "requested_teams": [{"name": "Core", "slug": "core"}]});
    let details = github_rest::details(&pull, ItemKind::Pr, 7, "o").unwrap();
    assert_eq!(details.author, "ghost");
    assert_eq!(details.pr.unwrap().reviewers, ["kim", "o/core"]);
}

#[test]
fn automatic_merges_read_their_method() {
    assert_eq!(auto_merge(&Value::Null).unwrap(), None);
    let request = json!({"merge_method": "squash", "enabled_by": {"login": "kim"}});
    assert_eq!(
        auto_merge(&request).unwrap(),
        Some(AutoMerge {
            method: Some(MergeMethod::Squash)
        })
    );
    assert!(auto_merge(&json!({"merge_method": "fast-forward"})).is_err());
}

#[test]
fn github_inline_comments_join_the_review_that_posted_them() {
    let reviews = [
        json!({"id": 1, "user": {"login": "bot"}, "state": "CHANGES_REQUESTED", "body": "Fix it", "submitted_at": "t1"}),
        json!({"id": 2, "user": {"login": "me"}, "state": "PENDING", "body": "", "submitted_at": null}),
        json!({"id": 3, "user": {"login": "me"}, "state": "COMMENTED", "body": "", "submitted_at": "t2"}),
    ];
    let comments = [
        json!({"id": 10, "pull_request_review_id": 1, "user": {"login": "bot"}, "path": "a.rs", "line": 3, "original_line": 2, "body": "Bug", "created_at": "t1"}),
        json!({"id": 11, "in_reply_to_id": 10, "pull_request_review_id": 3, "user": {"login": "me"}, "path": "a.rs", "line": null, "original_line": 2, "body": "Fixed", "created_at": "t2"}),
        json!({"id": 12, "pull_request_review_id": 3, "user": {"login": "me"}, "path": "b.rs", "line": 1, "body": "New", "created_at": "t2"}),
    ];
    let threads = [GitHubThread {
        node: "T_1".into(),
        resolved: true,
        comments: vec![10, 11],
    }];
    let reviews = parse_reviews(&reviews, &comments, &threads).unwrap();
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].state, "changes_requested");
    let bug = &reviews[0].comments[0];
    assert_eq!((bug.id, bug.thread, bug.line), (10, 10, Some(3)));
    assert_eq!(bug.resolved, Some(true));
    let fixed = &reviews[1].comments[0];
    assert_eq!((fixed.body.as_str(), fixed.line), ("Fixed", Some(2)));
    assert_eq!(
        (fixed.id, fixed.thread, fixed.resolved),
        (11, 10, Some(true))
    );
    // A thread GraphQL did not list has unknown resolution.
    assert_eq!(reviews[1].comments[1].resolved, None);
    assert!(parse_reviews(&[json!({"id": 4, "state": "LATER"})], &[], &[]).is_err());
}
