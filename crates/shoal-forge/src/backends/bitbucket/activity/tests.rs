use serde_json::json;

use super::*;

#[test]
fn statuses_keep_the_newest_of_each_key() {
    let listed = json!([
        {"key": "ci", "name": "Pipeline", "state": "FAILED", "updated_on": "2026-01-01T00:00:00Z"},
        {"key": "ci", "name": "Pipeline", "state": "INPROGRESS", "updated_on": "2026-01-02T00:00:00Z"},
        {"key": "deploy", "name": "", "state": "STOPPED", "updated_on": "2026-01-01T00:00:00Z"}
    ]);
    let checks = statuses(listed.as_array().unwrap()).unwrap();
    assert_eq!(
        checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect::<Vec<_>>(),
        [
            ("Pipeline", CheckState::Pending),
            ("deploy", CheckState::Cancelled)
        ]
    );
    assert!(statuses(&[json!({"key": "x", "state": "LATER"})]).is_err());
}

#[test]
fn participants_approve_or_request_changes() {
    let pull = json!({"participants": [
        {"user": {"nickname": "ann"}, "role": "REVIEWER", "approved": true, "state": "approved"},
        {"user": {"nickname": "bob"}, "role": "REVIEWER", "approved": false, "state": "changes_requested"},
        {"user": {"display_name": "Cat"}, "role": "PARTICIPANT", "approved": false, "state": null},
        {"user": {"nickname": "dan"}, "role": "REVIEWER", "approved": true}
    ]});
    assert_eq!(
        verdicts(&pull).unwrap(),
        [
            ("ann".to_owned(), Verdict::Approved),
            ("bob".to_owned(), Verdict::ChangesRequested),
            ("dan".to_owned(), Verdict::Approved)
        ]
    );
    assert!(verdicts(&json!({})).is_err());
}

#[test]
fn inline_threads_become_reviews_with_their_resolution() {
    let listed = json!([
        {"id": 1, "user": {"nickname": "ann"}, "content": {"raw": "Looks fine"}, "created_on": "t1"},
        {"id": 2, "user": {"nickname": "bob"}, "content": {"raw": "Bug"}, "created_on": "t2",
            "inline": {"path": "a.rs", "to": 3, "from": null}, "resolution": {"type": "comment_resolution"}},
        {"id": 3, "user": {"nickname": "ann"}, "content": {"raw": "Fixed"}, "created_on": "t3",
            "parent": {"id": 2}, "inline": {"path": "a.rs", "to": 3}},
        {"id": 4, "user": {"nickname": "bob"}, "content": {"raw": "Agreed"}, "created_on": "t4",
            "parent": {"id": 1}},
        {"id": 5, "user": {"nickname": "bob"}, "content": {"raw": ""}, "deleted": true}
    ]);
    let (comments, reviews) = threads(listed.as_array().unwrap());
    assert_eq!(
        comments
            .iter()
            .map(|c| (c.id.get(), c.body.as_str()))
            .collect::<Vec<_>>(),
        [(1, "Looks fine"), (4, "Agreed")]
    );
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].author, "bob");
    let thread = &reviews[0].comments;
    assert_eq!(
        thread
            .iter()
            .map(|c| (
                c.id.get(),
                c.thread.get(),
                c.body.as_str(),
                c.line,
                c.resolved
            ))
            .collect::<Vec<_>>(),
        [
            (2, 2, "Bug", Some(3), Some(true)),
            (3, 2, "Fixed", Some(3), Some(true))
        ]
    );
}
