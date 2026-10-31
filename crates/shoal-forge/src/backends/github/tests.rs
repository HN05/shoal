use super::*;

#[test]
fn searches_name_the_repository_kind_state_and_filters() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let search = list::Search {
        state: list::StateFilter::Closed,
        labels: vec!["good first issue".into()],
        author: Some("dev".into()),
        text: Some("crash in:title".into()),
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Pr, &search),
        "repo:team/repo is:pr is:closed label:\"good first issue\" author:dev crash in:title"
    );
    let all = list::Search {
        state: list::StateFilter::All,
        labels: Vec::new(),
        author: None,
        text: None,
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Issue, &all),
        "repo:team/repo is:issue"
    );
}

#[test]
fn graphql_errors_fail_a_successful_response() {
    let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
    assert_eq!(
        graphql_errors(&failed).unwrap(),
        "not allowed; GraphQL error"
    );
    assert_eq!(
        graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
        None
    );
    assert_eq!(graphql_errors(&serde_json::json!([])), None);
}

#[test]
fn reverts_go_through_graphql_and_report_the_new_branch() {
    let current = Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 7,
            url: String::new(),
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
    assert_eq!(reverted(&response).unwrap(), (9, "revert-7-topic".into()));
    assert!(reverted(&json!({"data": {"revertPullRequest": null}})).is_err());
}
