use super::*;
use anyhow::anyhow;
use serde_json::json;

#[test]
fn forgejo_details_distinguish_merged_prs_and_refuse_prs_as_issues() {
    let pull = json!({"number": 541, "title": "Share", "body": "Why", "state": "closed",
            "merged": true, "user": {"login": "dev"}, "labels": [{"name": "type/chore"}],
            "assignees": [{"login": "kim"}], "milestone": {"id": 5, "title": "v2"},
            "created_at": "2026-10-10T01:00:00+02:00", "head": {"ref": "topic"}, "base": {"ref": "main"},
            "draft": false, "pull_request": {"merged": true},
            "requested_reviewers": [{"login": "kim"}], "requested_reviewers_teams": [{"name": "core"}]});
    let details = github_rest::details(&pull, ItemKind::Pr, 541, "team").unwrap();
    assert_eq!(details.state, "merged");
    assert_eq!(details.labels, ["type/chore"]);
    assert_eq!(details.assignees, ["kim"]);
    assert_eq!(details.milestone.as_deref(), Some("v2"));
    let pr = details.pr.unwrap();
    assert_eq!(pr.head, "topic");
    assert_eq!(pr.reviewers, ["kim", "team/core"]);
    assert!(github_rest::details(&pull, ItemKind::Issue, 541, "team").is_err());

    let issue = json!({"number": 548, "title": "View", "body": "Text", "state": "open",
            "user": {"login": "HN05"}, "labels": [], "created_at": "t", "pull_request": null});
    let details = github_rest::details(&issue, ItemKind::Issue, 548, "team").unwrap();
    assert_eq!(
        (details.state.as_str(), details.body.as_str()),
        ("open", "Text")
    );
}

#[test]
fn the_latest_scheduling_in_the_timeline_sets_an_automatic_merge() {
    let event = |kind: &str| json!({"type": kind});
    assert_eq!(scheduled_merge(&[event("pull_push")]), None);
    let scheduled = [
        event("pull_scheduled_merge"),
        event("pull_cancel_scheduled_merge"),
        event("pull_scheduled_merge"),
        event("comment"),
    ];
    assert_eq!(
        scheduled_merge(&scheduled),
        Some(AutoMerge { method: None })
    );
    assert_eq!(scheduled_merge(&scheduled[..2]), None);
}

#[test]
fn forgejo_reviews_carry_inline_comments_and_resolution() {
    let reviews = [
        (
            json!({"id": 1, "user": {"login": "bot"}, "state": "COMMENT", "body": "Blocker", "submitted_at": "t1"}),
            Ok(json!([
                {"id": 20, "user": {"login": "bot"}, "path": "a.py", "position": 182, "original_position": 0,
                    "body": "Fetch first", "created_at": "t1", "resolver": {"login": "dev"}},
                {"id": 21, "user": {"login": "dev"}, "path": "a.py", "position": 182, "original_position": 0,
                    "body": "Done", "created_at": "t2", "resolver": null},
                {"id": 22, "user": {"login": "dev"}, "path": "a.py", "position": 0, "original_position": 9,
                    "body": "Old", "created_at": "t3", "resolver": null}
            ])),
        ),
        (
            json!({"id": 2, "user": {"login": "x"}, "state": "REQUEST_REVIEW", "body": ""}),
            Ok(json!([])),
        ),
        (
            json!({"id": 3, "user": {"login": "dev"}, "state": "COMMENT", "body": "", "submitted_at": "t4"}),
            Ok(json!([
                {"id": 23, "user": {"login": "dev"}, "path": "a.py", "position": 0, "original_position": 9,
                    "body": "Reply", "created_at": "t4", "resolver": null}
            ])),
        ),
    ];
    let mut errors = Vec::new();
    let reviews = parse_reviews(&reviews, &mut errors).unwrap();
    assert!(errors.is_empty());
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].state, "commented");
    let comments = &reviews[0].comments;
    let thread =
        |comment: &ReviewComment| (comment.id, comment.thread, comment.line, comment.resolved);
    assert_eq!(thread(&comments[0]), (20, 20, Some(182), Some(true)));
    assert_eq!(thread(&comments[1]), (21, 20, Some(182), Some(true)));
    assert_eq!(thread(&comments[2]), (22, 22, Some(9), Some(false)));
    // Another review's comment on the same line starts its own thread.
    assert_eq!(
        thread(&reviews[1].comments[0]),
        (23, 23, Some(9), Some(false))
    );
    let comments = github_rest::comments(&[
        json!({"id": 5, "user": {"login": "a"}, "body": "b", "created_at": "c"}),
    ]);
    assert_eq!((comments[0].id, comments[0].author.as_str()), (5, "a"));
}

#[test]
fn a_review_whose_comments_fail_keeps_its_body_and_the_others() {
    let reviews = [
        (
            json!({"id": 4, "user": {"login": "bot"}, "state": "COMMENT", "body": "Findings", "submitted_at": "t1"}),
            Err(anyhow!(
                "GET pulls/7/reviews/4/comments: connection refused"
            )),
        ),
        (
            json!({"id": 5, "user": {"login": "bot"}, "state": "APPROVED", "body": "Fine", "submitted_at": "t2"}),
            Ok(
                json!([{"id": 30, "user": {"login": "bot"}, "path": "a.py", "position": 1,
                    "original_position": 0, "body": "Nit", "created_at": "t2", "resolver": null}]),
            ),
        ),
    ];
    let mut errors = Vec::new();
    let reviews = parse_reviews(&reviews, &mut errors).unwrap();
    assert_eq!(
        errors,
        ["comments lookup failed for review 4: GET pulls/7/reviews/4/comments: connection refused"]
    );
    assert_eq!(
        (reviews[0].body.as_str(), reviews[0].comments.len()),
        ("Findings", 0)
    );
    assert_eq!(reviews[1].comments[0].id, 30);
}
