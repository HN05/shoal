use super::*;
use serde_json::json;

#[test]
fn pulls_read_the_same_from_both_forges() {
    let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let pull = json!({"number": 7, "title": "Fix", "state": "closed",
            "merged_at": "2026-01-01T00:00:00Z", "draft": false, "node_id": "PR_1",
            "merge_commit_sha": "abc",
            "labels": [{"name": "bug"}],
            "head": {"ref": "topic", "repo": {"full_name": "team/repo"}}, "base": {"ref": "main"}});
    let current = github.current(ItemKind::Pr, 7, &pull).unwrap();
    assert_eq!(
        current.item,
        Item {
            kind: ItemKind::Pr,
            number: 7,
            url: "https://github.com/team/repo/pull/7".into(),
            title: "Fix".into(),
            state: ItemState::Merged,
            labels: vec!["bug".into()],
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: false,
            }),
        }
    );
    assert_eq!(current.node_id.as_deref(), Some("PR_1"));
    assert_eq!(current.head_repository.as_deref(), Some("team/repo"));
    assert!(!current.merges_now);
    let mut clean = pull.clone();
    clean["mergeable_state"] = json!("clean");
    assert!(github.current(ItemKind::Pr, 7, &clean).unwrap().merges_now);

    assert_eq!(current.merge_commit.as_deref(), Some("abc"));

    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    // An open PR's merge commit is only the forge's test merge.
    let pull = json!({"number": 7, "title": "wip: Fix", "state": "open", "merged": false,
            "merge_commit_sha": "abc", "labels": [], "head": {"ref": "topic", "repo": null}, "base": {"ref": "main"}});
    let current = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
    assert_eq!(current.item.url, "https://forge.example/team/repo/pulls/7");
    assert_eq!(current.item.state, ItemState::Open);
    assert!(current.item.pr.unwrap().draft);
    assert_eq!(current.head_repository, None);
    assert_eq!(current.merge_commit, None);
}

#[test]
fn forgejo_pulls_keep_their_deleted_head_branch() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let pull = json!({"number": 7, "title": "Fix", "state": "closed", "merged": true,
            "head": {"ref": "refs/pull/7/head", "label": "topic"}, "base": {"ref": "main"}});
    let current = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
    assert_eq!(current.item.state, ItemState::Merged);
    assert_eq!(current.item.pr.unwrap().head, "topic");
}

#[test]
fn issue_reads_refuse_prs_and_other_numbers() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let issue = json!({"number": 3, "title": "Bug", "state": "open", "labels": [],
            "pull_request": null});
    let current = forgejo.current(ItemKind::Issue, 3, &issue).unwrap();
    assert_eq!(current.item.url, "https://forge.example/team/repo/issues/3");
    assert_eq!(current.item.pr, None);
    let pull = json!({"number": 3, "title": "Fix", "state": "open",
            "pull_request": {"merged": false}});
    assert!(forgejo.current(ItemKind::Issue, 3, &pull).is_err());
    assert!(forgejo.current(ItemKind::Issue, 4, &issue).is_err());
}

#[test]
fn issue_reads_tell_issues_from_prs() {
    let issue = json!({"number": 3, "pull_request": null});
    let pull = json!({"number": 4, "pull_request": {"merged": false}});
    assert_eq!(issue_read_kind(3, &issue).unwrap(), ItemKind::Issue);
    assert_eq!(issue_read_kind(4, &pull).unwrap(), ItemKind::Pr);
    assert!(issue_read_kind(5, &pull).is_err());
}
