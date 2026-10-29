use super::*;
use crate::forge::items::link::ItemKind;

#[test]
fn selected_numbers_name_one_kind() {
    let item = |kind, url: &str| SelectedItem {
        kind,
        url: url.into(),
    };
    let mut selected = Selected {
        path: PathBuf::new(),
        items: vec![
            item(ItemKind::Issue, "https://gitlab.com/team/repo/-/issues/5"),
            item(
                ItemKind::Pr,
                "https://gitlab.com/team/repo/-/merge_requests/6",
            ),
        ],
    };
    let remote = "git@gitlab.com:team/repo.git";
    assert_eq!(selected.kind_of(remote, 5).unwrap(), Some(ItemKind::Issue));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(ItemKind::Pr));
    assert_eq!(selected.kind_of(remote, 7).unwrap(), None);
    // An item of another repository is not the workspace's.
    selected.items.push(item(
        ItemKind::Issue,
        "https://github.com/them/tool/issues/6",
    ));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(ItemKind::Pr));
    selected.items.push(item(
        ItemKind::Pr,
        "https://gitlab.com/team/repo/-/merge_requests/5",
    ));
    assert!(selected.kind_of(remote, 5).is_err());
}

#[test]
fn linked_items_list_each_url_once_with_its_title() {
    let item = |kind, url: &str, title: Option<&str>| LinkedItem {
        kind,
        url: url.into(),
        title: title.map(Into::into),
    };
    let links = WorkspaceLinks {
        issue: Some("https://forge/o/r/issues/1".into()),
        issue_title: None,
        prs: vec!["https://forge/o/r/pulls/2".into()],
        opened: vec![
            item(ItemKind::Pr, "https://forge/o/r/pulls/2", Some("Fix it")),
            item(ItemKind::Issue, "https://forge/o/r/issues/3", None),
        ],
        related: vec![item(
            ItemKind::Issue,
            "https://forge/o/r/issues/1",
            Some("Broken"),
        )],
    };
    assert_eq!(
        links.items(),
        [
            item(
                ItemKind::Issue,
                "https://forge/o/r/issues/1",
                Some("Broken")
            ),
            item(ItemKind::Pr, "https://forge/o/r/pulls/2", Some("Fix it")),
            item(ItemKind::Issue, "https://forge/o/r/issues/3", None),
        ]
    );
}

#[test]
fn linked_items_know_their_repository() {
    let item = |kind, url: &str| LinkedItem {
        kind,
        url: url.into(),
        title: None,
    };
    let remote = "git@github.com:team/project.git";
    assert!(item(ItemKind::Pr, "https://github.com/team/project/pull/7").in_repository(remote));
    assert!(
        item(ItemKind::Issue, "https://github.com/team/project/issues/3").in_repository(remote)
    );
    assert!(!item(ItemKind::Issue, "https://github.com/them/tool/issues/3").in_repository(remote));
}
