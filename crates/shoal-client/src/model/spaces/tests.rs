use super::*;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceSelector;

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
        narrowing: Default::default(),
        mode: "chat".into(),
        number,
        id: id.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        agent: "claude".into(),
        workspace: None,
        pid: 1,
        started_at: crate::time::UnixTime::EPOCH,
        origin: None,
        tmp_dir: None,
        root_dir: None,
        session: None,
        restore,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        progress: None,
        parent: None,
        children: Vec::new(),
    };
    let spaces = Spaces {
        workspaces: vec![workspace],
        chats: vec![
            chat("c1", 1.into(), "talk", None),
            chat(
                "c2",
                2.into(),
                "saved",
                Some(crate::state::ChatRestore::Resume),
            ),
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
    let below: Vec<SpaceId> = (spaces.below(&SpaceId::from("talk")).into_iter())
        .map(|space| space.id)
        .collect();
    assert_eq!(below, ["opened", "planning", "grandchild"]);
    assert!(spaces.below(&SpaceId::from("other")).is_empty());
}
