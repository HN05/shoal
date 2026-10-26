use super::*;

#[test]
fn added_exclusive_labels_replace_those_of_their_scope() {
    let edit = Edit {
        add_labels: vec![
            "complexity/medium".into(),
            "area/cli".into(),
            "type/bug".into(),
        ],
        ..Edit::default()
    };
    let labels: Vec<String> = ["complexity/small", "area/daemon", "type/bug", "bug"]
        .map(str::to_owned)
        .into();
    let complexity = |name: &str| name.starts_with("complexity/");
    assert_eq!(
        replaced_labels(&edit, &labels, complexity),
        ["complexity/small"]
    );
    // Both labels must be exclusive.
    let small = |name: &str| name == "complexity/small";
    assert!(replaced_labels(&edit, &labels, small).is_empty());
    assert_eq!(
        replaced_labels(&edit, &labels, |_| true),
        ["complexity/small", "area/daemon"]
    );
    assert_eq!(exclusive_scope("a/b/c"), Some("a/b"));
    assert_eq!(exclusive_scope("/b"), None);
    assert_eq!(exclusive_scope("a/"), None);
    assert_eq!(exclusive_scope("bug"), None);
}

#[test]
fn searches_go_through_the_issue_list_for_either_kind() {
    let mut search = list::Search {
        state: list::StateFilter::All,
        labels: Vec::new(),
        author: None,
        text: None,
        limit: 30,
    };
    assert_eq!(
        search_endpoint(ItemKind::Issue, &search),
        "issues?type=issues&state=all"
    );
    search.labels = vec!["area/cli".into(), "bug".into()];
    search.author = Some("claude-dev".into());
    search.text = Some("watch timeout".into());
    assert_eq!(
        search_endpoint(ItemKind::Pr, &search),
        "issues?type=pulls&state=all&labels=area%2Fcli%2Cbug&created_by=claude-dev&q=watch%20timeout"
    );
}

#[test]
fn forgejo_statuses_need_every_status_and_known_states() {
    let combined = json!({"total_count": 2, "statuses": [
        {"context": "ci / rust", "status": "success"},
        {"context": "review", "status": "pending"}
    ]});
    assert_eq!(
        statuses(&combined).unwrap(),
        [
            RestCheck {
                name: "ci / rust".into(),
                state: "Success"
            },
            RestCheck {
                name: "review".into(),
                state: "Pending"
            },
        ]
    );
    assert!(
        statuses(&json!({"total_count": 0, "statuses": null}))
            .unwrap()
            .is_empty()
    );
    let partial = json!({"total_count": 3, "statuses": combined["statuses"]});
    assert!(statuses(&partial).is_err());
    let unknown = json!({"total_count": 1, "statuses": [{"context": "x", "status": "later"}]});
    assert!(statuses(&unknown).is_err());
}

#[test]
fn pages_end_at_a_short_page() {
    let full = serde_json::json!([1, 2]);
    assert!(Forgejo.page(&full, 2).unwrap().1);
    assert!(!Forgejo.page(&full, 3).unwrap().1);
}

#[test]
fn a_limit_without_a_page_reads_the_first_page() {
    for (endpoint, read) in [
        (
            "repos/t/r/actions/runs?limit=8",
            "repos/t/r/actions/runs?limit=8&page=1",
        ),
        (
            "repos/t/r/actions/runs?status=success&limit=8#x",
            "repos/t/r/actions/runs?status=success&limit=8&page=1#x",
        ),
        (
            "repos/t/r/actions/runs?limit=8&page=3",
            "repos/t/r/actions/runs?limit=8&page=3",
        ),
        ("repos/t/r/actions/runs", "repos/t/r/actions/runs"),
        (
            "repos/t/r/branches?unlimited=1",
            "repos/t/r/branches?unlimited=1",
        ),
    ] {
        assert_eq!(Forgejo.raw_read(endpoint.into()), read);
    }
}

#[test]
fn draft_prefixes_ignore_case_and_need_the_whole_marker() {
    assert_eq!(draft_prefix("WIP: x"), Some(4));
    assert_eq!(draft_prefix("[wip] x"), Some(5));
    assert_eq!(draft_prefix("Wipe x"), None);
    assert_eq!(draft_prefix("Ü"), None);
}
