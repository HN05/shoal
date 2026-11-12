use serde_json::json;

use super::*;
use crate::ci::check_state::CheckState;

#[test]
fn merges_when_the_pipeline_succeeds_keep_their_squash_setting() {
    let mut merge_request = json!({"merge_when_pipeline_succeeds": false, "squash": true});
    assert_eq!(auto_merge(&merge_request), None);
    merge_request["merge_when_pipeline_succeeds"] = json!(true);
    assert_eq!(
        auto_merge(&merge_request),
        Some(AutoMerge {
            method: Some(MergeMethod::Squash)
        })
    );
    merge_request["squash"] = json!(false);
    assert_eq!(
        auto_merge(&merge_request).and_then(|merge| merge.method),
        Some(MergeMethod::Merge)
    );
}

#[test]
fn approvals_and_reviewer_states_become_verdicts() {
    let reviewers = json!([
        {"user": {"username": "ann"}, "state": "requested_changes"},
        {"user": {"username": "bob"}, "state": "reviewed"},
        {"user": {"username": "cat"}, "state": "unreviewed"}
    ]);
    let approvals = json!({"approved_by": [{"user": {"username": "dan"}}]});
    assert_eq!(
        verdicts(&reviewers, &approvals).unwrap(),
        [
            ("ann".to_owned(), Verdict::ChangesRequested),
            ("bob".to_owned(), Verdict::Commented),
            ("dan".to_owned(), Verdict::Approved)
        ]
    );
    assert!(verdicts(&json!([{"state": "later"}]), &approvals).is_err());
    assert!(verdicts(&json!([]), &json!({})).is_err());
}

#[test]
fn diff_threads_become_reviews_and_other_notes_comments() {
    let listed = json!([
        {"notes": [{"system": true, "body": "added 1 commit"}]},
        {"notes": [{"id": 20, "author": {"username": "ann"}, "body": "Looks fine", "created_at": "t1"}]},
        {"notes": [
            {"id": 30, "type": "DiffNote", "author": {"username": "bob"}, "body": "Bug", "created_at": "t2",
                "resolved": false, "position": {"new_path": "a.rs", "new_line": 3}},
            {"id": 31, "type": "DiffNote", "author": null, "body": "Fixed", "created_at": "t3",
                "resolved": true, "position": {"old_path": "b.rs", "new_line": null, "old_line": 9}}
        ]}
    ]);
    let (comments, reviews) = discussions(listed.as_array().unwrap());
    assert_eq!(comments.len(), 1);
    assert_eq!(
        (
            comments[0].id.get(),
            comments[0].author.as_str(),
            comments[0].body.as_str()
        ),
        (20, "ann", "Looks fine")
    );
    assert_eq!(reviews.len(), 1);
    assert_eq!(
        (reviews[0].author.as_str(), reviews[0].state.as_str()),
        ("bob", "commented")
    );
    let threads = &reviews[0].comments;
    assert_eq!(
        (
            threads[0].path.as_str(),
            threads[0].line,
            threads[0].resolved
        ),
        ("a.rs", Some(3), Some(false))
    );
    assert_eq!(
        (
            threads[1].author.as_str(),
            threads[1].path.as_str(),
            threads[1].line
        ),
        ("ghost", "b.rs", Some(9))
    );
    assert_eq!(
        threads
            .iter()
            .map(|c| (c.id.get(), c.thread.get()))
            .collect::<Vec<_>>(),
        [(30, 30), (31, 30)]
    );
}

#[test]
fn statuses_keep_the_newest_of_each_name_in_shared_states() {
    let listed = json!([
        {"id": 1, "name": "rust", "status": "failed"},
        {"id": 3, "name": "rust", "status": "running"},
        {"id": 2, "name": "lint", "status": "failed", "allow_failure": true},
        {"id": 4, "name": "deploy", "status": "manual"},
        {"id": 5, "name": "external", "status": "canceled"},
        {"id": 6, "name": "docs", "status": "pending"}
    ]);
    let checks = statuses(listed.as_array().unwrap()).unwrap();
    assert_eq!(
        checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect::<Vec<_>>(),
        [
            ("deploy", CheckState::Skipped),
            ("docs", CheckState::Waiting),
            ("external", CheckState::Cancelled),
            ("lint", CheckState::Warning),
            ("rust", CheckState::Pending)
        ]
    );
    assert!(statuses(&[json!({"id": 1, "name": "x", "status": "later"})]).is_err());
}
