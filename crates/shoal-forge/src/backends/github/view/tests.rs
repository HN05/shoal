use super::*;
use serde_json::json;

#[test]
fn github_issue_and_pr_details_read_content_and_comments() {
    let issue = json!({"number": 4, "title": "Crash", "body": "Steps", "state": "OPEN",
            "author": {"login": "ann"}, "labels": [{"name": "bug"}], "createdAt": "2026-01-01T00:00:00Z",
            "comments": [{"author": {"login": "bob"}, "createdAt": "2026-01-02T00:00:00Z", "body": "Same here",
                "url": "https://github.com/o/r/issues/4#issuecomment-41"}]});
    let details = parse_details(&issue.to_string(), ItemKind::Issue, 4).unwrap();
    assert_eq!(details.title, "Crash");
    assert_eq!(details.state, "open");
    assert_eq!(details.author, "ann");
    assert_eq!(details.labels, ["bug"]);
    assert!(details.pr.is_none());
    let comment = &details.comments.unwrap()[0];
    assert_eq!((comment.id, comment.body.as_str()), (41, "Same here"));
    assert!(parse_details(&issue.to_string(), ItemKind::Issue, 5).is_err());

    let pull = json!({"number": 7, "title": "Fix", "body": "", "state": "MERGED",
            "author": null, "labels": [], "createdAt": "2026-01-01T00:00:00Z",
            "headRefName": "topic", "baseRefName": "main", "isDraft": true});
    let details = parse_details(&pull.to_string(), ItemKind::Pr, 7).unwrap();
    assert_eq!(details.state, "merged");
    assert_eq!(details.author, "ghost");
    assert!(details.comments.is_none());
    let pr = details.pr.unwrap();
    assert_eq!(
        (pr.head.as_str(), pr.base.as_str(), pr.draft),
        ("topic", "main", true)
    );
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
