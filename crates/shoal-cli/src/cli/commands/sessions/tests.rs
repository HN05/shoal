use super::*;

/// A view of another machine's workspace, running chat and stopped chat.
fn view() -> HubView {
    let space = |id: &str, kind: &str, running: Option<&str>| {
        serde_json::json!({
            "machine": "m-laptop", "id": id, "number": 1, "name": id, "kind": kind,
            "status": "working", "running": running, "agent": true, "reported_at": 0,
        })
    };
    serde_json::from_value(serde_json::json!({
        "machine": "m-desk",
        "machines": [
            {"id": "m-laptop", "kind": "machine", "name": "laptop", "joined_at": 0,
             "connected": true, "hub": false},
        ],
        "spaces": [
            space("work", "workspace", None),
            space("talk", "chat", Some("claude")),
            space("quiet", "chat", None),
        ],
    }))
    .unwrap()
}

#[test]
fn attach_returns_to_another_machine_s_workspaces_and_running_chats() {
    let ids = attachable_elsewhere(&view());
    assert_eq!(ids, [SpaceId::from("work"), SpaceId::from("talk")]);
}

#[test]
fn a_previous_space_another_machine_lists_is_attached_there() {
    let (listed, _) = listing(view(), &SpaceId::from("talk")).unwrap();
    assert_eq!(listed.machine.as_str(), "m-laptop");
    assert!(listing(view(), &SpaceId::from("here")).is_none());
}
