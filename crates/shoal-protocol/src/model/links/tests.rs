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

#[test]
fn links_of_an_earlier_release_keep_their_issue_as_the_first_assigned() {
    let url = "https://forge/o/r/issues/4";
    let earlier = serde_json::json!({"issue": url, "issue_title": "Fix it", "prs": []});
    let links: SpaceLinks = serde_json::from_value(earlier).unwrap();
    assert_eq!(
        links.issues,
        [LinkedItem {
            url: url.parse().unwrap(),
            title: Some("Fix it".into()),
        }]
    );
    let without: SpaceLinks =
        serde_json::from_value(serde_json::json!({"issue": null, "prs": []})).unwrap();
    assert!(without.issues.is_empty());
}

#[test]
fn links_of_an_earlier_release_keep_their_prs_without_titles() {
    let (earlier, titled) = ("https://forge/o/r/pulls/4", "https://forge/o/r/pulls/5");
    let links: SpaceLinks = serde_json::from_value(serde_json::json!({"prs": [
        earlier,
        {"kind": "pr", "url": titled, "title": "Fix it"},
    ]}))
    .unwrap();
    assert_eq!(
        links.prs,
        [
            LinkedItem {
                url: earlier.parse().unwrap(),
                title: None,
            },
            LinkedItem {
                url: titled.parse().unwrap(),
                title: Some("Fix it".into()),
            },
        ]
    );
}
