use serde_json::json;
use shoal_protocol::changes::ToolChange;

use super::change;

#[test]
fn a_patch_and_a_command_are_snapshotted_and_other_tools_change_nothing() {
    let event = |event: &str, tool: &str| {
        change(&json!({"hook_event_name": event, "tool_name": tool, "tool_use_id": "call_1"}))
    };
    let call = "call_1".to_owned();
    assert_eq!(
        event("PreToolUse", "apply_patch"),
        Some(ToolChange::Starts { call: call.clone() })
    );
    assert_eq!(
        event("PostToolUse", "Bash"),
        Some(ToolChange::Ends { call })
    );
    assert_eq!(event("PostToolUse", "update_plan"), None);
}
