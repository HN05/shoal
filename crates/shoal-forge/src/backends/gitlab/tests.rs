use super::*;

#[test]
fn open_blocking_issues_are_the_open_is_blocked_by_links() {
    let links = [
        json!({"link_type": "is_blocked_by", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/1"}),
        json!({"link_type": "is_blocked_by", "state": "closed", "web_url": "https://gitlab.com/g/p/-/issues/2"}),
        json!({"link_type": "blocks", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/3"}),
        json!({"link_type": "relates_to", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/4"}),
    ];
    assert_eq!(
        open_blocking(&links).unwrap(),
        ["https://gitlab.com/g/p/-/issues/1"]
    );
}

#[test]
fn closed_merge_request_searches_list_all_to_include_merged_ones() {
    let mut search = list::Search {
        state: list::StateFilter::Closed,
        labels: vec!["bug".into(), "ui".into()],
        author: Some("dev".into()),
        text: Some("crash on start".into()),
        limit: 30,
    };
    assert_eq!(
        search_endpoint(ItemKind::Pr, &search),
        "merge_requests?state=all&labels=bug%2Cui&author_username=dev&search=crash%20on%20start"
    );
    search.labels.clear();
    search.author = None;
    search.text = None;
    assert_eq!(
        search_endpoint(ItemKind::Issue, &search),
        "issues?state=closed"
    );
    search.state = list::StateFilter::Open;
    assert_eq!(
        search_endpoint(ItemKind::Pr, &search),
        "merge_requests?state=opened"
    );
}
use serde_json::json;

fn gitlab() -> ForgeRepo {
    ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap()
}

#[test]
fn merge_requests_read_branches_drafts_and_conflicts() {
    let merge_request = json!({"iid": 7, "title": "Draft: Fix", "state": "opened",
            "description": "Why", "labels": ["bug"], "source_branch": "topic",
            "target_branch": "main", "draft": true, "sha": "abc", "has_conflicts": true,
            "detailed_merge_status": "conflict", "source_project_id": 1, "target_project_id": 1});
    let current = gitlab().current(ItemKind::Pr, 7, &merge_request).unwrap();
    assert_eq!(
        current.item,
        Item {
            kind: ItemKind::Pr,
            number: 7,
            url: "https://gitlab.com/group/sub/project/-/merge_requests/7".into(),
            title: "Draft: Fix".into(),
            state: ItemState::Open,
            labels: vec!["bug".into()],
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: true,
            }),
        }
    );
    assert_eq!(current.conflict, Some(true));
    assert_eq!(current.head_sha.as_deref(), Some("abc"));
    assert_eq!(
        current.head_repository.as_deref(),
        Some("group/sub/project")
    );
    assert_eq!(current.body, "Why");

    let mut checking = merge_request.clone();
    checking["detailed_merge_status"] = json!("checking");
    checking["source_project_id"] = json!(2);
    let current = gitlab().current(ItemKind::Pr, 7, &checking).unwrap();
    assert_eq!(current.conflict, None);
    assert_eq!(current.head_repository, None);

    let mut merged = merge_request;
    merged["state"] = json!("merged");
    let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
    assert_eq!(current.item.state, ItemState::Merged);
    assert!(gitlab().current(ItemKind::Pr, 8, &merged).is_err());
}

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
fn statuses_keep_the_newest_of_each_name_in_shared_states() {
    let listed = json!([
        {"id": 1, "name": "rust", "status": "failed"},
        {"id": 3, "name": "rust", "status": "running"},
        {"id": 2, "name": "lint", "status": "failed", "allow_failure": true},
        {"id": 4, "name": "deploy", "status": "manual"},
        {"id": 5, "name": "external", "status": "canceled"}
    ]);
    let checks = statuses(listed.as_array().unwrap()).unwrap();
    assert_eq!(
        checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect::<Vec<_>>(),
        [
            ("deploy", "Skipped"),
            ("external", "Cancelled"),
            ("lint", "Warning"),
            ("rust", "Pending")
        ]
    );
    assert!(statuses(&[json!({"id": 1, "name": "x", "status": "later"})]).is_err());
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
            comments[0].id,
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
        threads.iter().map(|c| (c.id, c.thread)).collect::<Vec<_>>(),
        [(30, 30), (31, 30)]
    );
}

#[test]
fn notes_find_their_discussion() {
    let listed = json!([
        {"id": "d1", "notes": [{"id": 1, "system": true}, {"id": 2, "resolved": false}, {"id": 3}]},
        {"id": "d2", "notes": [{"id": 4, "resolved": true}]}
    ]);
    let listed = listed.as_array().unwrap();
    let thread = find_thread(listed, 3).unwrap();
    assert_eq!(
        (thread.first, thread.replies, thread.resolved),
        (2, 1, Some(false))
    );
    assert_eq!(
        thread.target,
        Target::GitLab {
            discussion: "d1".into()
        }
    );
    assert_eq!(find_thread(listed, 4).unwrap().resolved, Some(true));
    // System notes are events, not comments.
    assert!(find_thread(listed, 1).is_none());
}

#[test]
fn issues_use_their_own_numbers_routes_and_states() {
    let issue = json!({"iid": 3, "id": 900, "title": "Bug", "state": "closed",
            "labels": [{"name": "type/bug"}]});
    let current = gitlab().current(ItemKind::Issue, 3, &issue).unwrap();
    assert_eq!(
        current.item.url,
        "https://gitlab.com/group/sub/project/-/issues/3"
    );
    assert_eq!(current.item.state, ItemState::Closed);
    assert_eq!(current.item.labels, ["type/bug"]);
    assert!(gitlab().current(ItemKind::Issue, 900, &issue).is_err());
    let mut merged = issue;
    merged["state"] = json!("merged");
    assert!(gitlab().current(ItemKind::Issue, 3, &merged).is_err());
}

#[test]
fn retried_jobs_report_their_newest_attempt() {
    // A retried job's newest attempt decides; its failed first one is history.
    let gitlab = [
        json!({"id": 7, "name": "rust", "status": "failed", "target_url": "old"}),
        json!({"id": 9, "name": "rust", "status": "running", "target_url": "new"}),
        json!({"id": 8, "name": "docs", "status": "canceled"}),
        json!({"id": 10, "name": "deploy", "status": "waiting_for_callback"}),
    ];
    assert_eq!(
        latest_checks(&gitlab).unwrap(),
        [
            ("deploy".into(), JobState::Running, String::new()),
            ("docs".into(), JobState::Failed, String::new()),
            ("rust".into(), JobState::Running, "new".into())
        ]
    );
    let unknown = [json!({"id": 1, "name": "x", "status": "later"})];
    assert!(latest_checks(&unknown).is_err());
}
