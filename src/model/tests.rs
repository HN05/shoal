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
