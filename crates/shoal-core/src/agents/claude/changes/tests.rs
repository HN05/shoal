use serde_json::json;
use shoal_protocol::{changes::ToolChange, code::Lines};

use super::change;

#[test]
fn an_edit_reports_the_hunks_of_its_structured_patch() {
    let event = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_use_id": "toolu_1",
        "tool_input": {"file_path": "/work/parse.rs"},
        "tool_response": {
            "filePath": "/work/parse.rs",
            "originalFile": null,
            "structuredPatch": [{
                "oldStart": 9, "oldLines": 4, "newStart": 9, "newLines": 5,
                "lines": [" a", "-b", "+B", "+B2", " c", " d"],
            }],
        },
    });
    let Some(ToolChange::Edited { path, hunks }) = change(&event) else {
        panic!("{event}")
    };
    assert_eq!(path.to_str(), Some("/work/parse.rs"));
    assert_eq!(hunks.len(), 1);
    assert_eq!(
        (hunks[0].before, hunks[0].lines),
        (9, Lines { start: 9, end: 11 })
    );
}

#[test]
fn a_created_file_adds_every_line_and_a_command_is_snapshotted() {
    let created = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_response": {
            "type": "create", "filePath": "/work/new.rs",
            "content": "a\nb\n", "structuredPatch": [],
        },
    });
    let Some(ToolChange::Edited { hunks, .. }) = change(&created) else {
        panic!("{created}")
    };
    assert_eq!(hunks[0].added, ["a", "b"]);
    let command = |event: &str| {
        change(&json!({
            "hook_event_name": event, "tool_name": "Bash", "tool_use_id": "toolu_2",
        }))
    };
    let call = "toolu_2".to_owned();
    assert_eq!(
        command("PreToolUse"),
        Some(ToolChange::Starts { call: call.clone() })
    );
    assert_eq!(
        command("PostToolUseFailure"),
        Some(ToolChange::Ends { call })
    );
}
