use serde_json::json;

use super::*;

/// A `PostToolUse` event of an `update_plan` call, as Codex sends it.
fn event(input: Value) -> Value {
    json!({
        "session_id": "019a5c0e-7d1f-7b52-9c0b-3e4f5a6b7c8d",
        "transcript_path": "/Users/me/.codex/sessions/2026/11/08/rollout.jsonl",
        "cwd": "/Users/me/repo",
        "hook_event_name": "PostToolUse",
        "model": "gpt-5.5-codex",
        "turn_id": "1",
        "tool_name": "update_plan",
        "tool_use_id": "call_01",
        "tool_input": input,
        "tool_response": "Plan updated",
    })
}

#[test]
fn update_plan_replaces_the_plan_from_its_arguments_or_their_text() {
    let arguments = json!({
        "explanation": "Found the cause",
        "plan": [
            {"step": "Reproduce", "status": "completed"},
            {"step": "Fix the timeout", "status": "in_progress"},
            {"step": "Add a test", "status": "pending"},
        ],
    });
    let expected = Some(ProgressChange::Steps(vec![
        PlanStep {
            name: "Reproduce".into(),
            state: StepState::Done,
            key: None,
        },
        PlanStep {
            name: "Fix the timeout".into(),
            state: StepState::Active,
            key: None,
        },
        PlanStep {
            name: "Add a test".into(),
            state: StepState::Pending,
            key: None,
        },
    ]));
    assert_eq!(change(&event(arguments.clone())), expected);
    assert_eq!(change(&event(json!(arguments.to_string()))), expected);
    let mut shell = event(json!({"command": ["ls"]}));
    shell["tool_name"] = json!("shell");
    assert_eq!(change(&shell), None);
}
