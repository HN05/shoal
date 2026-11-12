use serde_json::json;

use super::*;
use crate::numbers::CommentId;

fn gitlab() -> ForgeRepo {
    ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap()
}

#[test]
fn comment_edits_replace_the_body_of_an_items_note() {
    let repo = gitlab();
    let pull = repo.item_endpoint(ItemKind::Pr, 7);
    let inline = GitLab
        .edit_comment(
            &repo,
            ItemKind::Pr,
            7,
            Subject::ReviewComment(CommentId::new(40)),
            "Why?",
        )
        .unwrap();
    assert_eq!(
        inline,
        Request::new(
            HttpMethod::Put,
            format!("{pull}/notes/40"),
            Some(json!({"body": "Why?"})),
        )
    );
    let issue = GitLab
        .edit_comment(
            &repo,
            ItemKind::Issue,
            3,
            Subject::Comment(CommentId::new(41)),
            "Fixed",
        )
        .unwrap();
    assert_eq!(
        issue.endpoint,
        format!("{}/notes/41", repo.item_endpoint(ItemKind::Issue, 3))
    );
    assert!(
        GitLab
            .edit_comment(&repo, ItemKind::Pr, 7, Subject::Item, "Body")
            .is_err()
    );
}

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
            url: "https://gitlab.com/group/sub/project/-/merge_requests/7"
                .parse()
                .unwrap(),
            title: "Draft: Fix".into(),
            state: ItemState::Open,
            labels: vec!["bug".into()],
            assignees: Vec::new(),
            milestone: None,
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: true,
            }),
        }
    );
    assert_eq!(current.conflict, Some(true));
    assert_eq!(current.head_sha, Some("abc".into()));
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

    assert_eq!(current.merge_commit, None);

    let mut merged = merge_request;
    merged["state"] = json!("merged");
    merged["sha"] = json!("head");
    let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
    assert_eq!(current.item.state, ItemState::Merged);
    // A fast-forward merge leaves the source branch's head on the target.
    assert_eq!(current.merge_commit, Some("head".into()));
    merged["squash_commit_sha"] = json!("squashed");
    let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
    assert_eq!(current.merge_commit, Some("squashed".into()));
    assert!(gitlab().current(ItemKind::Pr, 8, &merged).is_err());
}

#[test]
fn issues_use_their_own_numbers_routes_and_states() {
    let issue = json!({"iid": 3, "id": 900, "title": "Bug", "state": "closed",
            "labels": [{"name": "type/bug"}], "assignees": [{"id": 4, "username": "kim"}],
            "milestone": {"id": 40, "iid": 1, "title": "v2"}});
    let current = gitlab().current(ItemKind::Issue, 3, &issue).unwrap();
    assert_eq!(
        current.item.url.url(),
        "https://gitlab.com/group/sub/project/-/issues/3"
    );
    assert_eq!(current.item.state, ItemState::Closed);
    assert_eq!(current.item.labels, ["type/bug"]);
    assert_eq!(current.item.assignees, ["kim"]);
    assert_eq!(current.item.milestone.as_deref(), Some("v2"));
    assert_eq!(current.user_ids.get("kim"), Some(&4));
    assert!(gitlab().current(ItemKind::Issue, 900, &issue).is_err());
    let mut merged = issue;
    merged["state"] = json!("merged");
    assert!(gitlab().current(ItemKind::Issue, 3, &merged).is_err());
}
