use super::*;
use crate::space_names::SpaceSelector;
use shoal_ids::space_ids::SpaceId;

/// A ready workspace `id` on the branch `id`.
fn workspace(id: &str) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "repository_id": "repo",
        "name": id,
        "path": format!("/{id}"),
        "branch": id,
        "state": "ready",
        "workdir": {"kind": "worktree", "path": format!("/{id}"),
                    "workspace": {"id": id, "name": id, "branch": id}},
    }))
    .unwrap()
}

/// A chat `id` in the repository's checkout.
fn chat(id: &str) -> Chat {
    serde_json::from_value(serde_json::json!({
        "id": id, "repository_id": "repo", "name": id, "agent": "claude",
        "mode": "chat", "started_at": 1,
        "workdir": {"kind": "checkout", "path": "/repo"},
    }))
    .unwrap()
}

#[test]
fn a_workspace_and_a_chat_read_back_as_spaces_of_their_kind() {
    let workspace = serde_json::to_value(workspace("w1")).unwrap();
    assert_eq!(workspace["kind"], "workspace");
    assert_eq!(workspace["workdir"]["kind"], "worktree");
    assert_eq!(workspace["workdir"]["path"], "/w1");
    let chat = serde_json::to_value(chat("c1")).unwrap();
    assert_eq!(chat["kind"], "chat");
    assert_eq!(chat["workdir"]["kind"], "checkout");

    let workspace: Space = serde_json::from_value(workspace).unwrap();
    assert_eq!(workspace.kind(), SpaceKind::Workspace);
    // A space's own JSON names its kind once and reads back the same.
    let space = serde_json::to_string(&workspace).unwrap();
    assert_eq!(
        space.matches("\"kind\":\"workspace\"").count(),
        1,
        "{space}"
    );
    let workspace: Space = serde_json::from_str(&space).unwrap();
    let workspace = workspace.into_workspace().unwrap();
    assert_eq!(workspace.path, std::path::Path::new("/w1"));
    let chat: Space = serde_json::from_value(chat).unwrap();
    assert!(chat.worktree().is_none());
    assert_eq!(chat.into_chat().unwrap().agent, Some("claude".into()));
}

#[test]
fn spaces_find_a_workspace_or_a_running_or_stopped_chat_by_number_or_id() {
    let mut worker = workspace("w1");
    worker.name = "worker".into();
    worker.number = 3.into();
    let chat = |id: &str, number: u32, name: &str, running| Chat {
        number: number.into(),
        name: name.into(),
        running,
        ..chat(id)
    };
    let spaces = Spaces {
        workspaces: vec![worker],
        chats: vec![chat("c1", 1, "talk", true), chat("c2", 2, "saved", false)],
    };
    let found = |selector: &str| {
        let space = spaces.find(&SpaceSelector::from(selector));
        space.map(|space| space.name().to_owned())
    };
    assert_eq!(found("w1").as_deref(), Some("worker"));
    assert_eq!(found("worker"), None);
    assert_eq!(found("3").as_deref(), Some("worker"));
    assert_eq!(found("c1").as_deref(), Some("talk"));
    assert_eq!(found("2").as_deref(), Some("saved"));
    assert_eq!(found("saved"), None);
    assert_eq!(found("4"), None);
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
        Workspace {
            children,
            ..workspace(id)
        }
    };
    let chat = |id: &str, children: Vec<SpaceRef>| -> Chat {
        Chat {
            children,
            ..chat(id)
        }
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
