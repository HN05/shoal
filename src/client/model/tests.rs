use super::*;
use crate::forge::items::link::LinkKind;

#[test]
fn selected_numbers_name_one_kind() {
    let item = |kind, url: &str| SelectedItem {
        kind,
        url: url.into(),
    };
    let mut selected = Selected {
        path: PathBuf::new(),
        items: vec![
            item(LinkKind::Issue, "https://gitlab.com/team/repo/-/issues/5"),
            item(
                LinkKind::Pr,
                "https://gitlab.com/team/repo/-/merge_requests/6",
            ),
        ],
    };
    let remote = "git@gitlab.com:team/repo.git";
    assert_eq!(selected.kind_of(remote, 5).unwrap(), Some(LinkKind::Issue));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(LinkKind::Pr));
    assert_eq!(selected.kind_of(remote, 7).unwrap(), None);
    // An item of another repository is not the workspace's.
    selected.items.push(item(
        LinkKind::Issue,
        "https://github.com/them/tool/issues/6",
    ));
    assert_eq!(selected.kind_of(remote, 6).unwrap(), Some(LinkKind::Pr));
    selected.items.push(item(
        LinkKind::Pr,
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
    let links = SpaceLinks {
        issue: Some("https://forge/o/r/issues/1".into()),
        issue_title: None,
        prs: vec!["https://forge/o/r/pulls/2".into()],
        opened: vec![
            item(LinkKind::Pr, "https://forge/o/r/pulls/2", Some("Fix it")),
            item(LinkKind::Issue, "https://forge/o/r/issues/3", None),
        ],
        related: vec![item(
            LinkKind::Issue,
            "https://forge/o/r/issues/1",
            Some("Broken"),
        )],
    };
    assert_eq!(
        links.items(),
        [
            item(
                LinkKind::Issue,
                "https://forge/o/r/issues/1",
                Some("Broken")
            ),
            item(LinkKind::Pr, "https://forge/o/r/pulls/2", Some("Fix it")),
            item(LinkKind::Issue, "https://forge/o/r/issues/3", None),
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
    assert!(item(LinkKind::Pr, "https://github.com/team/project/pull/7").in_repository(remote));
    assert!(
        item(LinkKind::Issue, "https://github.com/team/project/issues/3").in_repository(remote)
    );
    assert!(!item(LinkKind::Issue, "https://github.com/them/tool/issues/3").in_repository(remote));
}

#[test]
fn spaces_find_a_workspace_or_a_running_or_saved_chat_by_id_or_name() {
    let workspace: Workspace = serde_json::from_value(serde_json::json!({
        "id": "w1",
        "repository_id": "repo",
        "name": "worker",
        "path": "/worker",
        "branch": "worker",
        "state": "ready",
    }))
    .unwrap();
    let chat = |id: &str, name: &str, restore| Chat {
        id: id.into(),
        repository_id: "repo".into(),
        name: name.into(),
        agent: "claude".into(),
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir: None,
        session: None,
        restore,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
    };
    let spaces = Spaces {
        workspaces: vec![workspace],
        chats: vec![
            chat("c1", "talk", None),
            chat("c2", "saved", Some(crate::state::ChatRestore::Resume)),
        ],
    };
    let found = |selector| spaces.find(selector).map(|space| space.name().to_owned());
    assert_eq!(found("w1").as_deref(), Some("worker"));
    assert_eq!(found("worker").as_deref(), Some("worker"));
    assert_eq!(found("c1").as_deref(), Some("talk"));
    assert_eq!(found("saved").as_deref(), Some("saved"));
    assert!(spaces.find("missing").is_none());
}
