use super::*;
use crate::transfer::ManifestExt;
use shoal_ids::agent_session_ids::AgentSessionId;

/// The chat `id` of repository `r1`, below `parent` when given and in the
/// worktree of `workspace` when given, whose agent reported `s-<id>`.
fn chat(id: &str, parent: Option<&str>, workspace: Option<&str>) -> Chat {
    let space = |id: &str, kind: &str| serde_json::json!({"id": id, "name": id, "kind": kind});
    serde_json::from_value(serde_json::json!({
        "id": id, "repository_id": "r1", "name": id, "agent": "claude", "started_at": 0,
        "session": format!("s-{id}"),
        "parent": parent.map(|parent| space(parent, "chat")),
        "workspace": workspace.map(|workspace| serde_json::json!({"id": workspace, "name": workspace, "branch": null})),
    }))
    .unwrap()
}

#[test]
fn a_space_carries_the_chats_in_its_worktree_and_below_it_through_chats() {
    let chats = [
        // Below a chat it carries, listed before that chat.
        chat("grandchild", Some("reading"), None),
        // In its worktree, below a space that stays.
        chat("reading", Some("elsewhere"), Some("w1")),
        chat("child", Some("w1"), None),
        // In the worktree of a workspace below it, which carries it.
        chat("split-reading", None, Some("w2")),
        chat("other", None, None),
    ];

    let carried = carried_chats(&chats, &"w1".into());

    assert_eq!(
        carried.iter().map(SpaceId::as_str).collect::<Vec<_>>(),
        ["reading", "grandchild", "child"]
    );
}

#[test]
fn a_carried_chat_keeps_its_session_only_where_its_agent_continues_it() {
    let removed: RemovedSpace = serde_json::from_value(serde_json::json!({
        "workspace_id": "c1", "repository_id": "r1", "name": "reading",
        "links": {"issues": [], "prs": []},
        "last_sessions": [{"execution": "c1", "agent": "claude", "session": "s1"}],
        "cause": "manual", "removed_at": 1, "expires_at": 2,
        "chat": {"agent": "claude", "mode": "chat",
            "origin": {"host": "herdr", "place": "tab"}},
    }))
    .unwrap();
    let carried = CarriedChat::of(None, Some("m1".into()), removed);
    // The record carries no place it ran in.
    assert_eq!(carried.chat.chat().unwrap().origin, None);
    let manifest = crate::transfer::Manifest {
        remote_url: None,
        space: crate::transfer::PackagedSpace {
            prerequisite: None,
            bundle: false,
            record: serde_json::from_value(serde_json::json!({
                "workspace_id": "lead", "repository_id": null, "name": "lead",
                "links": {"issues": [], "prs": []},
                "cause": "manual", "removed_at": 1, "expires_at": 2,
                "chat": {"agent": "claude", "mode": "chat"},
            }))
            .unwrap(),
        },
        sessions: None,
        session_files: Vec::new(),
        copy: None,
        chats: vec![carried.clone()],
    };
    let session = |machine| {
        let sessions = manifest.chat_sessions_on(&carried, Some(machine), None);
        let chat = carried.chat_with(sessions);
        (chat.last_sessions.first()).and_then(|last| last.session.clone())
    };

    let s1: AgentSessionId = "s1".into();
    assert_eq!(session("m1"), Some(s1));
    assert_eq!(session("m2"), None);
    assert_eq!(manifest.every_session().reported().count(), 1);
}
