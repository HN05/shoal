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
