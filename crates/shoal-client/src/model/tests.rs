use super::*;
use crate::forge::items::link::LinkKind;
use crate::space_names::SpaceSelector;

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
fn spaces_find_a_workspace_or_a_running_or_stopped_chat_by_number_id_or_name() {
    let workspace: Workspace = serde_json::from_value(serde_json::json!({
        "id": "w1",
        "number": 3,
        "repository_id": "repo",
        "name": "worker",
        "path": "/worker",
        "branch": "worker",
        "state": "ready",
    }))
    .unwrap();
    let chat = |id: &str, number, name: &str, restore| Chat {
        mode: "chat".into(),
        number,
        id: id.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        agent: "claude".into(),
        workspace: None,
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir: None,
        root_dir: None,
        session: None,
        restore,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        parent: None,
        children: Vec::new(),
    };
    let spaces = Spaces {
        workspaces: vec![workspace],
        chats: vec![
            chat("c1", 1, "talk", None),
            chat("c2", 2, "saved", Some(crate::state::ChatRestore::Resume)),
        ],
    };
    let found = |selector: &str| {
        let space = spaces.find(&SpaceSelector::parse(selector));
        space.map(|space| space.name().to_owned())
    };
    assert_eq!(found("w1").as_deref(), Some("worker"));
    assert_eq!(found("worker").as_deref(), Some("worker"));
    assert_eq!(found("3").as_deref(), Some("worker"));
    assert_eq!(found("c1").as_deref(), Some("talk"));
    assert_eq!(found("2").as_deref(), Some("saved"));
    assert_eq!(found("saved").as_deref(), Some("saved"));
    assert_eq!(found("4"), None);
    assert_eq!(found("missing"), None);
}

#[test]
fn the_spaces_below_a_space_include_those_below_them() {
    let space = |id: &str, kind: SpaceKind| SpaceRef {
        id: id.into(),
        name: id.into(),
        kind,
        branch: (kind == SpaceKind::Workspace).then(|| id.into()),
    };
    let workspace = |id: &str, children: Vec<SpaceRef>| -> Workspace {
        let mut workspace: Workspace = serde_json::from_value(serde_json::json!({
            "id": id,
            "repository_id": "repo",
            "name": id,
            "path": format!("/{id}"),
            "branch": id,
            "state": "ready",
        }))
        .unwrap();
        workspace.children = children;
        workspace
    };
    let chat = |id: &str, children: Vec<SpaceRef>| -> Chat {
        let mut chat: Chat = serde_json::from_value(serde_json::json!({
            "id": id, "repository_id": "repo", "name": id, "agent": "claude",
            "pid": 1, "started_at": 1,
        }))
        .unwrap();
        chat.children = children;
        chat
    };
    let spaces = Spaces {
        workspaces: vec![
            workspace("opened", vec![space("planning", SpaceKind::Chat)]),
            workspace("grandchild", Vec::new()),
            workspace("other", Vec::new()),
        ],
        chats: vec![
            chat("talk", vec![space("opened", SpaceKind::Workspace)]),
            chat("planning", vec![space("grandchild", SpaceKind::Workspace)]),
        ],
    };
    let below: Vec<String> = (spaces.below("talk").into_iter())
        .map(|space| space.id)
        .collect();
    assert_eq!(below, ["opened", "planning", "grandchild"]);
    assert!(spaces.below("other").is_empty());
}

#[test]
fn merge_preview_names_its_direction_push_warning_and_open_prs() {
    let mut preview = MergePreview {
        workspace_id: "id".into(),
        branch: "fix-login".into(),
        head: "abc".into(),
        default_branch: "main".into(),
        default_commit: "def".into(),
        commits: 1,
        push_to: None,
        unpushed: 0,
        warning: None,
        open_prs: Vec::new(),
    };
    let description = preview.description();
    assert!(description.contains("(1 commit)"), "{description}");
    assert!(!description.contains("push"), "{description}");
    preview.commits = 3;
    preview.push_to = Some(MergePush {
        remote: "origin".into(),
        reference: "refs/heads/main".into(),
    });
    preview.warning = Some(MergeWarning {
        subject: "fix-login".into(),
        target: "main".into(),
        files: vec!["version".into()],
        url: None,
    });
    preview.open_prs = vec!["https://forge/pulls/12".into()];
    let description = preview.description();
    for part in [
        "(3 commits)",
        "push main to origin.",
        ": version.",
        "https://forge/pulls/12",
    ] {
        assert!(description.contains(part), "{part:?} in {description}");
    }
    assert_eq!(preview.reason(), description.replace('\n', " "));
    preview.warning = None;
    preview.open_prs.clear();
    preview.unpushed = 2;
    let description = preview.description();
    assert!(description.contains("2 commits"), "{description}");
    assert!(description.contains("origin/main"), "{description}");
    preview.unpushed = 0;
    preview.push_to.as_mut().unwrap().reference = "refs/heads/release".into();
    assert!(
        preview
            .description()
            .ends_with("push main to origin/release.")
    );
    preview.branch = "b".repeat(300);
    assert_eq!(preview.reason().len(), crate::validate::MAX_REASON_LEN);
    assert!(preview.reason().ends_with('…'));
}
