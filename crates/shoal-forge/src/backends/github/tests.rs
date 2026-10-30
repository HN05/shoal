use super::*;

#[test]
fn lists_keep_filter_values_attached_to_their_flags() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let search = list::Search {
        state: list::StateFilter::Closed,
        labels: vec!["-odd".into()],
        author: Some("dev".into()),
        text: Some("crash in:title".into()),
        limit: 5,
    };
    assert_eq!(
        list_args(&repo, ItemKind::Pr, &search),
        [
            "pr",
            "list",
            "--repo=github.com/team/repo",
            "--state=closed",
            "--limit=5",
            "--json=number,title,state,labels,headRefName,baseRefName,isDraft",
            "--label=-odd",
            "--author=dev",
            "--search=crash in:title",
        ]
    );
    let listed: ListedItem = serde_json::from_value(json!({"number": 4, "title": "Fix",
            "state": "MERGED", "labels": [{"name": "bug"}], "headRefName": "topic",
            "baseRefName": "main", "isDraft": false}))
    .unwrap();
    let item = listed.into_item(&repo, ItemKind::Pr).unwrap();
    assert_eq!(item.state, ItemState::Merged);
    assert_eq!(item.url, "https://github.com/team/repo/pull/4");
    assert_eq!(item.pr.unwrap().head, "topic");
}

#[test]
fn query_errors_preserve_login_guidance_and_diagnostic_limits() {
    use std::{io, os::unix::process::ExitStatusExt, process::Output};

    for tool in ["gh", "fj"] {
        for (query, limit, hint) in [
            (
                Query::Issue,
                shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                "auth login",
            ),
            (
                Query::Pull(MERGED_HINT),
                shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                "shoal done",
            ),
            (
                Query::Pull(""),
                shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                "existing login",
            ),
        ] {
            let missing = query
                .response(tool, Err(io::Error::from(io::ErrorKind::NotFound)))
                .unwrap_err();
            let missing = format!("{missing:#}");
            assert!(missing.contains(tool), "{missing}");
            assert!(missing.contains(hint), "{missing}");

            let failed = query
                .response(
                    tool,
                    Ok(Output {
                        status: std::process::ExitStatus::from_raw(256),
                        stdout: b"ignored".to_vec(),
                        stderr: format!("{}END", "é".repeat(limit)).into_bytes(),
                    }),
                )
                .unwrap_err();
            let failed = format!("{failed:#}");
            assert!(failed.contains(tool), "{failed}");
            assert!(failed.contains(hint), "{failed}");
            assert!(failed.ends_with(&"é".repeat(limit)));
            assert!(!failed.contains("END"));

            let invalid = query
                .response(
                    tool,
                    Ok(Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: vec![0xff],
                        stderr: Vec::new(),
                    }),
                )
                .unwrap_err();
            assert!(format!("{invalid:#}").contains("output is not UTF-8"));
        }
    }
}

#[test]
fn github_requires_matching_number_and_known_state() {
    for (state, expected) in [("OPEN", ItemState::Open), ("CLOSED", ItemState::Closed)] {
        let output = serde_json::json!({"number": 316, "state": state}).to_string();
        assert_eq!(reported_state(&output, 316).unwrap(), expected);
        assert!(reported_state(&output, 1).is_err());
    }
    for output in [
        r#"{"number":316,"state":"MERGED"}"#,
        r#"{"number":316}"#,
        "",
    ] {
        assert!(reported_state(output, 316).is_err());
    }
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
