use super::*;

/// A workspace event of `kind` as the journal stores it, with `extra` fields.
fn event(kind: &str, extra: serde_json::Value) -> LogEntry {
    let mut record = serde_json::json!({
        "type": "event", "id": 7, "created_at": 1, "kind": kind,
        "workspace_id": "w", "chat_id": null, "repository_id": "r", "name": "fix",
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
        event("agent_exited", serde_json::json!({})),
        event("review_ready", serde_json::json!({})),
        event("item_activity", activity("merged")),
        event("child_removed", serde_json::json!({})),
    ];
    let hidden = [
        event(
            "agent_state",
            serde_json::json!({"agent_state": {"state": "idle"}}),
        ),
        event("item_activity", activity("comment")),
        event("branch_changed", serde_json::json!({})),
        event("demo_started", serde_json::json!({})),
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
    assert_eq!(value["workspace_id"], "w");
    assert!(value["chat_id"].is_null());
    assert_eq!(value["agent"]["exit_code"], 3);
    let chat: LogEntry = serde_json::from_value(serde_json::json!({
        "type": "chat", "id": 8, "created_at": 2, "kind": "chat_renamed",
        "chat": {"id": "c", "repository_id": null, "name": "helper", "agent": "claude",
            "pid": 1, "started_at": 1},
    }))
    .unwrap();
    assert_eq!(
        (chat.id(), chat.created_at(), chat.is_entry()),
        (8, 2, true)
    );
}
