use super::*;

fn event(record: serde_json::Value) -> LogEntry {
    let mut base = serde_json::json!({
        "type": "event", "id": 1, "created_at": 1, "workspace_id": "w", "chat_id": null,
        "repository_id": "r", "name": "fix", "path": "/fix", "branch": "fix",
        "cause": null, "error": null,
    });
    base.as_object_mut()
        .unwrap()
        .extend(record.as_object().unwrap().clone());
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
        details(serde_json::json!({"kind": "renamed", "name": "fix-login",
            "branch": "fix/login"})),
        "fix-login, branch fix/login"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "renamed", "title": "Login retries"})),
        "Login retries"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "agent_exited",
            "agent": {"name": "claude", "execution_id": "e", "exit_code": 2}})),
        "claude, exit code 2"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "review_ready", "review": {"kind": "pr",
            "url": "https://forge.example/team/repo/pulls/2131", "head": "4e1c9a2f00"}})
        ),
        "#2131 at 4e1c9a2"
    );
    assert_eq!(
        details(serde_json::json!({"kind": "linked", "link": {"kind": "ci",
            "url": "https://forge.example/team/repo/actions/runs/2765", "relation": "related"}})),
        "run 2765 related"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "question_answered", "question": {
            "space_number": 3, "item": 2, "question": "Which database?", "answer": "SQLite"}})
        ),
        "3.2 SQLite"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "job_ended", "job": {"name": "rust",
            "run": "https://forge.example/team/repo/actions/runs/2765",
            "stopped": [{"pid": 41, "command": "sh -c loop"}, {"pid": 42, "command": "sleep 60"}]}})
        ),
        "rust, run 2765; stopped 41 sh -c loop, 42 sleep 60"
    );
    assert_eq!(
        details(
            serde_json::json!({"kind": "job_ended", "job": {"name": "rust",
            "run": "https://forge.example/team/repo/actions/runs/2765",
            "stopped": [{"pid": 41, "command": "sh -c loop"}], "omitted": 3}})
        ),
        "rust, run 2765; stopped 41 sh -c loop, 3 more"
    );
}

#[test]
fn shoal_s_and_the_forge_s_changes_name_the_item_and_change() {
    let (label, details) = describe(&event(serde_json::json!({"kind": "item_changed",
        "item": {"kind": "pr", "url": "https://forge.example/team/repo/pulls/7", "action": "open"}})));
    assert_eq!((label.as_str(), details.as_str()), ("PR opened", "#7"));
    let (label, details) = describe(&event(serde_json::json!({"kind": "item_changed",
        "item": {"kind": "pr", "url": "https://forge.example/team/repo/pulls/7",
            "action": "delete_branch"}})));
    assert_eq!(
        (label.as_str(), details.as_str()),
        ("PR changed", "#7 delete branch")
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
