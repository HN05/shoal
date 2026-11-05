use super::*;

/// A chat named `name`, with a session to continue, in the worktree of
/// `workspace` when given.
fn chat(name: &str, workspace: Option<&str>) -> Chat {
    serde_json::from_value(serde_json::json!({
        "id": name, "repository_id": "repo", "name": name, "agent": "claude",
        "pid": 1, "started_at": 0, "session": "session",
        "workspace": workspace.map(|workspace| serde_json::json!({
            "id": workspace, "name": workspace, "branch": workspace,
        })),
    }))
    .unwrap()
}

#[test]
fn a_chat_in_a_workspace_is_refused_before_anything_ends() {
    check_chat_export(chat("talk", None)).unwrap();
    assert!(check_chat_export(chat("reading", Some("worker"))).is_err());
}

#[test]
fn a_workspace_with_chats_in_it_is_refused_before_anything_ends() {
    let workspace = crate::model::Workspace::new_record(
        "repo".into(),
        "worker".into(),
        "/work/worker".into(),
        Some("worker".into()),
        crate::state::WorkspaceState::Ready,
    );
    let space = |name: &str, kind| crate::model::SpaceRef {
        id: name.into(),
        name: name.into(),
        kind,
        branch: None,
    };
    let check: RemovalCheck = serde_json::from_value(serde_json::json!({
        "workspace": workspace, "running_commands": 0, "processes": [], "dirty": false,
        "unpushed_commits": 0, "branch": "worker", "matches_default_branch": true,
        "children": [
            space("split", crate::model::SpaceKind::Workspace),
            space("reading", crate::model::SpaceKind::Chat),
        ],
    }))
    .unwrap();
    // Exporting the split workspace with it does not move the chat.
    let error = ensure_children_exported(&check, &["worker", "split"]).unwrap_err();
    assert!(error.to_string().contains("(reading)"), "{error}");
}
