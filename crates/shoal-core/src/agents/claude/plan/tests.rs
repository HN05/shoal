use serde_json::json;

use super::*;

/// A `PostToolUse` event of a plan tool call, as Claude Code sends it.
fn event(tool: &str, input: Value, response: Value) -> Value {
    json!({
        "session_id": "5d0c6f0e-6a64-4c39-9a8a-2f4f5b7e1c11",
        "transcript_path": "/Users/me/.claude/projects/repo/5d0c6f0e.jsonl",
        "cwd": "/Users/me/repo",
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": input,
        "tool_response": response,
        "tool_use_id": "toolu_01",
    })
}

#[test]
fn todo_write_replaces_the_plan() {
    let todos = json!([
        {"content": "Reproduce", "status": "completed", "activeForm": "Reproducing"},
        {"content": "Fix the timeout", "status": "in_progress", "activeForm": "Fixing"},
        {"content": "Add a test", "status": "pending", "activeForm": "Adding a test"},
    ]);
    let input = json!({ "todos": todos });
    let response = json!({ "oldTodos": [], "newTodos": todos });
    let Some(ProgressChange::Steps(steps)) = change(&event("TodoWrite", input, response)) else {
        panic!("TodoWrite replaces the steps");
    };
    let states: Vec<_> = (steps.iter())
        .map(|step| (step.name.as_str(), step.state))
        .collect();
    assert_eq!(
        states,
        [
            ("Reproduce", StepState::Done),
            ("Fix the timeout", StepState::Active),
            ("Add a test", StepState::Pending),
        ]
    );
}

#[test]
fn tasks_are_created_and_changed_by_their_id() {
    let input = json!({"subject": "Fix the timeout", "description": "Raise it to 30 seconds"});
    let response = json!({"task": {"id": "2", "subject": "Fix the timeout"}});
    assert_eq!(
        change(&event("TaskCreate", input, response)),
        Some(ProgressChange::StepCreated(PlanStep {
            name: "Fix the timeout".into(),
            state: StepState::Pending,
            key: Some("2".into()),
        }))
    );
    let input = json!({"taskId": "2", "status": "in_progress"});
    let response = json!({"success": true, "taskId": "2", "updatedFields": ["status"]});
    assert_eq!(
        change(&event("TaskUpdate", input, response)),
        Some(ProgressChange::StepChanged {
            key: "2".into(),
            name: None,
            state: Some(StepState::Active),
            removed: false,
        })
    );
    let input = json!({"taskId": "2", "status": "deleted"});
    let deleted = change(&event(
        "TaskUpdate",
        input.clone(),
        json!({"success": true}),
    ));
    assert!(matches!(
        deleted,
        Some(ProgressChange::StepChanged { removed: true, .. })
    ));
    let failed = json!({"success": false, "error": "Task not found"});
    assert_eq!(change(&event("TaskUpdate", input, failed)), None);
}

#[test]
fn a_subagents_plan_and_other_tools_change_nothing() {
    let mut subagent = event(
        "TodoWrite",
        json!({"todos": [{"content": "Search", "status": "pending"}]}),
        json!({}),
    );
    subagent["agent_id"] = json!("a1b2c3");
    subagent["agent_type"] = json!("Explore");
    assert_eq!(change(&subagent), None);
    let bash = event("Bash", json!({"command": "ls"}), json!({"stdout": ""}));
    assert_eq!(change(&bash), None);
}
