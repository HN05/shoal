use super::*;
use crate::forge::items::link::LinkKind;

#[test]
fn selected_numbers_name_one_kind() {
    let item = |url: &str| SelectedItem {
        url: url.parse().unwrap(),
        relations: Vec::new(),
    };
    let mut selected = Selected {
        path: PathBuf::new(),
        items: vec![
            item("https://gitlab.com/team/repo/-/issues/5"),
            item("https://gitlab.com/team/repo/-/merge_requests/6"),
        ],
    };
    let remote = "git@gitlab.com:team/repo.git";
    assert_eq!(selected.kind_of(remote, 5).unwrap(), Some(LinkKind::Issue));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(LinkKind::Pr));
    assert_eq!(selected.kind_of(remote, 7).unwrap(), None);
    // An item of another repository is not the workspace's.
    selected
        .items
        .push(item("https://github.com/them/tool/issues/6"));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(LinkKind::Pr));
    selected
        .items
        .push(item("https://gitlab.com/team/repo/-/merge_requests/5"));
    assert!(selected.kind_of(remote, 5).is_err());
}

#[test]
fn linked_items_list_each_url_once_with_its_title() {
    let item = |url: &str, title: Option<&str>| LinkedItem {
        url: url.parse().unwrap(),
        title: title.map(Into::into),
    };
    let links = SpaceLinks {
        issue: Some("https://forge/o/r/issues/1".parse().unwrap()),
        issue_title: None,
        prs: vec!["https://forge/o/r/pulls/2".parse().unwrap()],
        opened: vec![
            item("https://forge/o/r/pulls/2", Some("Fix it")),
            item("https://forge/o/r/issues/3", None),
        ],
        related: vec![item("https://forge/o/r/issues/1", Some("Broken"))],
    };
    assert_eq!(
        links.items(),
        [
            item("https://forge/o/r/issues/1", Some("Broken")),
            item("https://forge/o/r/pulls/2", Some("Fix it")),
            item("https://forge/o/r/issues/3", None),
        ]
    );
}

#[test]
fn linked_items_know_their_repository() {
    let item = |url: &str| LinkedItem {
        url: url.parse().unwrap(),
        title: None,
    };
    let remote = "git@github.com:team/project.git";
    assert!(item("https://github.com/team/project/pull/7").in_repository(remote));
    assert!(item("https://github.com/team/project/issues/3").in_repository(remote));
    assert!(!item("https://github.com/them/tool/issues/3").in_repository(remote));
}

#[test]
fn a_linked_item_keeps_its_kind_beside_its_url_and_refuses_another() {
    let pull = "https://github.com/team/project/pull/7";
    let item = LinkedItem {
        url: pull.parse().unwrap(),
        title: Some("Fix it".into()),
    };
    let json = serde_json::json!({"kind": "pr", "url": pull, "title": "Fix it"});
    assert_eq!(serde_json::to_value(&item).unwrap(), json);
    assert_eq!(serde_json::from_value::<LinkedItem>(json).unwrap(), item);
    let wrong = serde_json::json!({"kind": "issue", "url": pull});
    assert!(serde_json::from_value::<LinkedItem>(wrong).is_err());
}
