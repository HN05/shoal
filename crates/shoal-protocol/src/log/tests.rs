use super::*;

/// A workspace's event of `kind` as the journal stores it, with `extra`
/// fields.
fn event(kind: &str, extra: serde_json::Value) -> LogEntry {
    let mut record = serde_json::json!({
        "type": "event", "id": 7, "created_at": 1, "kind": kind,
        "space_id": "w", "space_kind": "workspace", "repository_id": "r", "name": "fix",
        "path": "/fix", "branch": "fix", "cause": null, "error": null,
    });
    record
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(record).unwrap()
}

#[test]
fn the_log_shows_lifecycle_work_and_coordination_but_not_turns_or_comments() {
    let activity = |kind| {
        serde_json::json!({"item_activity": {"kind": "pr", "url": "u",
        "update": {"kind": kind, "message": "m"}}})
    };
    let shown = [
        event("created", serde_json::json!({})),
        event(
            "agent_exited",
            serde_json::json!({"agent": {"name": "claude", "execution_id": "e", "exit_code": 0}}),
        ),
        event(
            "review_ready",
            serde_json::json!({"review": {"kind": null, "url": null}}),
        ),
        event("item_activity", activity("merged")),
        event(
            "child_removed",
            serde_json::json!({"child": {"id": "c", "name": "child", "kind": "chat"}}),
        ),
    ];
    let hidden = [
        event(
            "agent_state",
            serde_json::json!({"agent_state": {"state": "idle"}}),
        ),
        event("item_activity", activity("comment")),
        event("branch_changed", serde_json::json!({})),
        event(
            "demo_started",
            serde_json::json!({"demo": {"name": "web", "preset": null, "udid": null}}),
        ),
    ];
    assert!(shown.iter().all(LogEntry::is_entry));
    assert!(!hidden.iter().any(LogEntry::is_entry));
}

#[test]
fn an_entry_keeps_the_journal_lines_json() {
    let entry = event(
        "agent_exited",
        serde_json::json!({"agent": {"name": "claude", "execution_id": "e", "exit_code": 3}}),
    );
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["type"], "event");
    assert_eq!(value["id"], 7);
    assert_eq!(value["space_id"], "w");
    assert_eq!(value["space_kind"], "workspace");
    assert_eq!(value["agent"]["exit_code"], 3);
}

#[test]
fn only_a_workspace_s_entry_has_a_path_and_branch() {
    let workspace = serde_json::to_value(event("created", serde_json::json!({}))).unwrap();
    assert_eq!(
        (&workspace["path"], &workspace["branch"]),
        (&"/fix".into(), &"fix".into())
    );
    let chat = serde_json::json!({
        "type": "event", "id": 7, "created_at": 1, "kind": "created",
        "space_id": "c", "space_kind": "chat", "repository_id": null, "name": "plan",
        "cause": null, "error": null, "base_ref": null, "base_workspace": null, "parent": null,
    });
    let entry: LogEntry = serde_json::from_value(chat.clone()).unwrap();
    assert_eq!(serde_json::to_value(entry).unwrap(), chat);
}
