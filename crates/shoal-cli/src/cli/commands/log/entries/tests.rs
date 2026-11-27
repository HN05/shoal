use super::*;

fn event(record: serde_json::Value) -> LogEntry {
    let mut base = serde_json::json!({
        "type": "event", "id": 1, "created_at": 1, "space_id": "w", "space_kind": "workspace",
        "repository_id": "r", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null,
    });
    let fields = base.as_object_mut().unwrap();
    fields.extend(record.as_object().unwrap().clone());
    if fields["space_kind"] == "chat" {
        fields.retain(|key, _| key != "path" && key != "branch");
    }
    serde_json::from_value(base).unwrap()
}

fn details(record: serde_json::Value) -> String {
    describe(&event(record)).1
}

#[test]
fn details_name_what_each_entry_is_about() {
    assert_eq!(
        details(
            serde_json::json!({"kind": "created", "base_ref": "refs/heads/main",
            "parent": {"id": "p", "name": "plan-auth", "kind": "workspace"}})
        ),
        "from main, parent plan-auth"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "created", "base_ref": "refs/heads/main",
            "base_workspace": {"id": "b", "name": "api", "branch": "api-part"}})
        ),
        "from api-part"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "renamed", "name": "Login retries",
            "branch": "fix/login"})
        ),
        "Login retries"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "agent_started",
            "agent": {"name": "claude", "execution_id": "e", "exit_code": null,
            "source": {"kind": "trigger", "name": "nightly"}}})),
        "claude, trigger nightly"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "agent_exited",
            "agent": {"name": "claude", "execution_id": "e", "exit_code": 2}})),
        "claude, exit code 2"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "agent_exited",
            "agent": {"name": "claude", "execution_id": "e", "exit_code": 137, "oom_kills": 2}})),
        "claude, exit code 137, 2 killed for memory"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "agent_stopped",
            "agent": {"name": "claude", "execution_id": "e", "exit_code": null,
            "cause": {"kind": "signal", "signal": "SIGTERM", "reached": "wrapper"}}})),
        "claude, SIGTERM to the wrapper"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "review_ready", "review": {"kind": "pr",
            "url": "https://forge.example/team/repo/pulls/2131"}})
        ),
        "#2131"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "linked", "link": {"kind": "ci",
            "url": "https://forge.example/team/repo/actions/runs/2765", "relation": "related"}})),
        "run 2765 related"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "thread_posted", "thread": {
            "space_number": 3, "item": 2, "name": "Which database?", "sender": "fix",
            "text": "SQLite"}})),
        "3.2 Which database?: fix: SQLite"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "alert_sent",
            "alert": {"message": "Rebase onto\nmain", "alerts": [12, 13]}})),
        "12, 13 Rebase onto main"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "thread_forwarded", "thread": {
            "space_number": 3, "item": 2, "name": "Which database?", "sender": "fix",
            "to": "the user", "text": ""}})),
        "3.2 Which database?: fix to the user"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "thread_resolved", "thread": {
            "space_number": 3, "item": 2, "name": "Which database?"}})),
        "3.2 Which database?: reopened"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "job_ended", "job": {"name": "rust",
            "run": "https://forge.example/team/repo/actions/runs/2765",
            "reason": "its runner outlived its finished run",
            "stopped": [{"pid": 41, "command": "sh -c loop"}, {"pid": 42, "command": "sleep 60"}]}})
        ),
        "rust, run 2765; its runner outlived its finished run; stopped 41 sh -c loop, 42 sleep 60"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "job_ended", "job": {"name": "rust",
            "run": "https://forge.example/team/repo/actions/runs/2765",
            "reason": "stopped for critical memory pressure",
            "stopped": [{"pid": 41, "command": "sh -c loop"}], "omitted": 3}})
        ),
        "rust, run 2765; stopped for critical memory pressure; stopped 41 sh -c loop, 3 more"
    );
}

#[test]
fn a_conversion_names_what_the_space_became() {
    let converted = |record: serde_json::Value| {
        let mut record = record;
        record["kind"] = "converted".into();
        details(record)
    };
    assert_eq!(
        converted(serde_json::json!({})),
        "to workspace on fix at /fix"
    );
    assert_eq!(
        converted(serde_json::json!({"branch": null})),
        "to workspace at /fix"
    );
    assert_eq!(
        converted(serde_json::json!({"space_kind": "chat"})),
        "to chat"
    );
    assert_eq!(
        converted(serde_json::json!({"handoff": {
            "from": {"agent": "claude", "session": "c1", "integration": "claude"},
            "to": {"agent": "codex", "session": "x1", "integration": "codex"}}})),
        "from claude session c1 to codex session x1"
    );
}

#[test]
fn shoal_s_and_the_forge_s_changes_name_the_item_and_change() {
    let (label, details) = describe(&event(serde_json::json!({"kind": "item_changed",
        "item": {"kind": "pr", "url": "https://forge.example/team/repo/pulls/7", "action": "open"}})));
    assert_eq!((label.as_str(), details.as_str()), ("PR opened", "#7"));
    let (label, details) = describe(&event(serde_json::json!({"kind": "item_changed",
        "item": {"kind": "pr", "url": "https://forge.example/team/repo/pulls/7",
            "action": "cancel_auto_merge"}})));
    assert_eq!(
        (label.as_str(), details.as_str()),
        ("PR changed", "#7 cancel auto merge")
    );
    let (label, details) = describe(&event(serde_json::json!({"kind": "item_changed",
        "item": {"kind": "pr", "url": "https://forge.example/team/repo/pulls/7",
            "action": "request_changes"}})));
    assert_eq!(
        (label.as_str(), details.as_str()),
        ("PR changes requested", "#7")
    );
    let (label, _) = describe(&event(serde_json::json!({"kind": "item_activity",
        "item_activity": {"kind": "issue", "url": "https://forge.example/team/repo/issues/3",
            "update": {"kind": "closed", "message": "Closed"}}})));
    assert_eq!(label, "Issue closed");
}

#[test]
fn a_cleanup_run_shows_its_removals_and_its_failure() {
    let run = |record: serde_json::Value| {
        let mut base = serde_json::json!({"type": "shoal", "id": 1, "created_at": 1,
            "kind": "cleanup_ran"});
        base.as_object_mut()
            .unwrap()
            .extend(record.as_object().unwrap().clone());
        describe(&serde_json::from_value(base).unwrap())
    };
    let (label, details) = run(serde_json::json!({"removed": ["fix", "chat"], "error": "denied"}));
    assert_eq!(label, "Cleanup removed");
    assert_eq!(details, "fix, chat; failed: denied");
    let (label, details) = run(serde_json::json!({"error": "denied"}));
    assert_eq!(label, "Cleanup failed");
    assert_eq!(details, "denied");
}
