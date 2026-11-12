use super::*;
use crate::items::item::{Item, ItemState};

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
        author: None,
        head_sha: None,
        base_sha: None,
        merge_commit: None,
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        milestone_id: None,
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
