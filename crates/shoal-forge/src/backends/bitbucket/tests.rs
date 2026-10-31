use super::*;
use serde_json::json;

#[test]
fn searches_query_states_text_and_author_newest_first() {
    let mut search = list::Search {
        state: list::StateFilter::Open,
        labels: Vec::new(),
        author: None,
        text: None,
        limit: 30,
    };
    let decoded = |kind, search: &list::Search| {
        let endpoint = search_endpoint(kind, search);
        let (path, query) = endpoint.split_once("&q=").unwrap_or((&endpoint, ""));
        (path.to_owned(), percent_decoded(query))
    };
    assert_eq!(
        decoded(ItemKind::Issue, &search),
        (
            "issues?sort=-created_on".into(),
            r#"(state="new" OR state="open" OR state="on hold")"#.into()
        )
    );
    assert_eq!(
        decoded(ItemKind::Pr, &search),
        (
            "pullrequests?sort=-created_on&state=OPEN".into(),
            String::new()
        )
    );
    search.state = list::StateFilter::Closed;
    search.text = Some(r#"say "hi""#.into());
    search.author = Some("dev".into());
    assert_eq!(
        decoded(ItemKind::Pr, &search),
        (
            "pullrequests?sort=-created_on&state=MERGED&state=DECLINED&state=SUPERSEDED".into(),
            r#"(title ~ "say \"hi\"" OR description ~ "say \"hi\"") AND author.nickname="dev""#
                .into()
        )
    );
    search.state = list::StateFilter::All;
    assert_eq!(
        decoded(ItemKind::Issue, &search).1,
        r#"(title ~ "say \"hi\"" OR content.raw ~ "say \"hi\"") AND reporter.nickname="dev""#
    );
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
            decoded.push(u8::from_str_radix(hex, 16).unwrap());
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap()
}

fn bitbucket() -> ForgeRepo {
    ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap()
}

#[test]
fn pages_end_without_a_next_link() {
    let linked = json!({"values": [1], "next": "https://api.bitbucket.org/2.0/x?page=2"});
    assert_eq!(
        Bitbucket.page(&linked, 50).unwrap(),
        (&[1.into()][..], true)
    );
    assert!(!Bitbucket.page(&json!({"values": [1]}), 1).unwrap().1);
    assert!(Bitbucket.page(&json!([1, 2]), 2).is_none());
}

#[test]
fn tokens_are_bearer_or_basic_with_an_email() {
    assert_eq!(Bitbucket.authorization("abc", None), "Bearer abc");
    assert_eq!(
        Bitbucket.authorization("abc", Some("me@example.com")),
        format!("Basic {}", BASE64.encode("me@example.com:abc"))
    );
}

#[test]
fn pull_requests_read_branches_drafts_and_reviewers() {
    let pull = json!({"id": 7, "title": "Fix", "state": "DECLINED", "description": "Why",
            "draft": true, "reviewers": [{"account_id": "557058:abc", "nickname": "kim"}],
            "source": {"branch": {"name": "topic"}, "commit": {"hash": "abc123"},
                "repository": {"full_name": "workspace/repo"}},
            "destination": {"branch": {"name": "main"}}});
    let current = bitbucket().current(ItemKind::Pr, 7, &pull).unwrap();
    assert_eq!(
        current.item,
        Item {
            kind: ItemKind::Pr,
            number: 7,
            url: "https://bitbucket.org/workspace/repo/pull-requests/7".into(),
            title: "Fix".into(),
            state: ItemState::Closed,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: true,
            }),
        }
    );
    assert_eq!(current.reviewers, ["557058:abc"]);
    assert_eq!(current.head_repository.as_deref(), Some("workspace/repo"));
    assert_eq!(current.body, "Why");
    assert!(bitbucket().current(ItemKind::Pr, 8, &pull).is_err());
}

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
        [("Pipeline", "Pending"), ("deploy", "Cancelled")]
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
            .map(|c| (c.id, c.body.as_str()))
            .collect::<Vec<_>>(),
        [(1, "Looks fine"), (4, "Agreed")]
    );
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].author, "bob");
    let thread = &reviews[0].comments;
    assert_eq!(
        thread
            .iter()
            .map(|c| (c.id, c.thread, c.body.as_str(), c.line, c.resolved))
            .collect::<Vec<_>>(),
        [
            (2, 2, "Bug", Some(3), Some(true)),
            (3, 2, "Fixed", Some(3), Some(true))
        ]
    );
}

#[test]
fn replies_find_the_thread_they_belong_to() {
    let listed = json!([
        {"id": 2, "resolution": {"type": "comment_resolution"}},
        {"id": 3, "parent": {"id": 2}},
        {"id": 4, "parent": {"id": 3}},
        {"id": 5, "parent": {"id": 2}, "deleted": true},
        {"id": 6}
    ]);
    let listed = listed.as_array().unwrap();
    let thread = find_thread(listed, 4).unwrap();
    assert_eq!(
        (thread.first, thread.replies, thread.resolved),
        (2, 2, Some(true))
    );
    assert_eq!(find_thread(listed, 6).unwrap().resolved, Some(false));
    assert!(find_thread(listed, 5).is_none());
}

#[test]
fn changes_keep_the_title_and_cannot_reopen_pull_requests() {
    let mut current = bitbucket()
            .current(
                ItemKind::Pr,
                7,
                &json!({"id": 7, "title": "Fix", "state": "OPEN",
                    "reviewers": [{"account_id": "a"}, {"account_id": "b"}],
                    "source": {"branch": {"name": "topic"}}, "destination": {"branch": {"name": "main"}}}),
            )
            .unwrap();
    let edit = Edit {
        body: Some("Why".into()),
        base: Some("next".into()),
        draft: Some(true),
        add_reviewers: vec!["c".into()],
        remove_reviewers: vec!["a".into()],
        ..Edit::default()
    };
    let requests = Bitbucket.edit(&bitbucket(), &current, &edit).unwrap();
    assert_eq!(
        requests,
        [Request::new(
            HttpMethod::Put,
            "repositories/workspace/repo/pullrequests/7".into(),
            Some(json!({"title": "Fix", "description": "Why", "draft": true,
                    "destination": {"branch": {"name": "next"}},
                    "reviewers": [{"account_id": "b"}, {"account_id": "c"}]})),
        )]
    );
    let labels = Edit {
        add_labels: vec!["bug".into()],
        ..Edit::default()
    };
    assert!(Bitbucket.edit(&bitbucket(), &current, &labels).is_err());
    assert_eq!(
        Bitbucket
            .set_state(&bitbucket(), &current.item, ItemState::Closed)
            .unwrap()
            .endpoint,
        "repositories/workspace/repo/pullrequests/7/decline"
    );
    current.item.state = ItemState::Closed;
    assert!(
        Bitbucket
            .set_state(&bitbucket(), &current.item, ItemState::Open)
            .is_err()
    );
    current.item.kind = ItemKind::Issue;
    current.item.pr = None;
    assert_eq!(
        Bitbucket
            .set_state(&bitbucket(), &current.item, ItemState::Open)
            .unwrap()
            .body,
        Some(json!({"state": "open"}))
    );
    assert_eq!(
        merge_request("merge".into(), MergeMethod::Rebase, true).body,
        Some(json!({"merge_strategy": "rebase_fast_forward", "close_source_branch": true}))
    );
}

#[test]
fn issue_states_fold_into_open_and_closed() {
    for (state, expected) in [
        ("new", ItemState::Open),
        ("on hold", ItemState::Open),
        ("wontfix", ItemState::Closed),
        ("resolved", ItemState::Closed),
    ] {
        let issue = json!({"id": 3, "title": "Bug", "state": state, "content": {"raw": "Text"}});
        let current = bitbucket().current(ItemKind::Issue, 3, &issue).unwrap();
        assert_eq!(current.item.state, expected);
        assert_eq!(current.body, "Text");
    }
    let issue = json!({"id": 3, "title": "Bug", "state": "OPEN"});
    assert!(bitbucket().current(ItemKind::Issue, 3, &issue).is_err());
}

#[test]
fn issues_read_their_assignee_by_account_id_and_milestone_by_name() {
    let issue = json!({"id": 3, "title": "Bug", "state": "new",
            "assignee": {"account_id": "557058:abc", "nickname": "kim"},
            "milestone": {"id": 2, "name": "v2"}});
    let current = bitbucket().current(ItemKind::Issue, 3, &issue).unwrap();
    assert_eq!(current.item.assignees, ["557058:abc"]);
    assert_eq!(current.item.milestone.as_deref(), Some("v2"));
    let bare =
        json!({"id": 3, "title": "Bug", "state": "new", "assignee": null, "milestone": null});
    let current = bitbucket().current(ItemKind::Issue, 3, &bare).unwrap();
    assert!(current.item.assignees.is_empty() && current.item.milestone.is_none());
}

#[test]
fn comment_edits_replace_the_raw_content_of_inline_and_issue_comments() {
    let edit = |kind, number, subject| {
        Bitbucket
            .edit_comment(&bitbucket(), kind, number, subject, "Why?")
            .unwrap()
    };
    assert_eq!(
        edit(ItemKind::Pr, 7, Subject::ReviewComment(40)),
        Request::new(
            HttpMethod::Put,
            "repositories/workspace/repo/pullrequests/7/comments/40".into(),
            Some(json!({"content": {"raw": "Why?"}})),
        )
    );
    assert_eq!(
        edit(ItemKind::Issue, 3, Subject::Comment(41)).endpoint,
        "repositories/workspace/repo/issues/3/comments/41"
    );
}

#[test]
fn comment_authors_are_the_login_by_uuid_not_nickname() {
    let user = json!({"uuid": "{1}", "nickname": "sam"});
    let namesake = json!({"uuid": "{2}", "nickname": "sam"});
    assert!(same_account(&user, &json!({"uuid": "{1}", "nickname": "sam"})).unwrap());
    assert!(!same_account(&user, &namesake).unwrap());
    // A deleted account's comment names no user.
    assert!(!same_account(&user, &Value::Null).unwrap());
    assert!(same_account(&json!({"nickname": "sam"}), &namesake).is_err());
}

#[tokio::test]
async fn only_bitbucket_org_reaches_the_cloud_api() {
    let other = ForgeRepo::host(
        "bitbucket.example",
        "https",
        crate::backends::ForgeKind::Bitbucket,
    );
    let request = Request::get("user".into());
    let error = Bitbucket
        .send(&other, &Account::default(), &request)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("bitbucket.example is not Bitbucket Cloud"),
        "{error:#}"
    );
}
