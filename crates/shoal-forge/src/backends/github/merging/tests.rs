use super::*;
use shoal_forge_types::items::item::{Item, ItemState};

#[test]
fn reverts_go_through_graphql_and_report_the_new_branch() {
    let current = Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 7,
            url: "https://forge.example/team/repo/pulls/1".parse().unwrap(),
            title: "Fix".into(),
            state: ItemState::Merged,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            pr: None,
        },
        node_id: Some("PR_7".into()),
        head_repository: None,
        body: String::new(),
        comments: None,
        author: None,
        head_sha: None,
        base_sha: None,
        merge_commit: None,
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        milestone_id: None,
        merge_style: None,
        replaced_labels: Vec::new(),
    };
    let revert = NewRevert {
        title: "Revert \"Fix\"".into(),
        body: "Reverts #7".into(),
        draft: true,
        ..NewRevert::default()
    };
    let request = revert_request(&current, &revert).unwrap();
    assert_eq!(request.endpoint, "graphql");
    let body = request.body.unwrap();
    assert!(
        body["query"]
            .as_str()
            .unwrap()
            .contains("revertPullRequest(")
    );
    assert_eq!(
        body["variables"],
        json!({"id": "PR_7", "title": "Revert \"Fix\"", "body": "Reverts #7", "draft": true})
    );
    let response = json!({"data": {"revertPullRequest": {"revertPullRequest":
        {"number": 9, "headRefName": "revert-7-topic"}}}});
    assert_eq!(
        reverted(&response).unwrap(),
        (PullNumber::new(9), "revert-7-topic".into())
    );
    assert!(reverted(&json!({"data": {"revertPullRequest": null}})).is_err());
}

#[test]
fn a_merge_without_a_method_takes_the_first_one_the_repository_allows() {
    let allowed = |merge, squash, rebase| {
        allowed_merge(&json!({
            "allow_merge_commit": merge,
            "allow_squash_merge": squash,
            "allow_rebase_merge": rebase,
        }))
    };
    assert_eq!(allowed(true, true, true), Some(MergeMethod::Merge));
    assert_eq!(allowed(false, true, true), Some(MergeMethod::Squash));
    assert_eq!(allowed(false, false, true), Some(MergeMethod::Rebase));
    // An account that cannot see the settings reads none of them.
    assert_eq!(allowed_merge(&json!({})), None);
}
