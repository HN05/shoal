//! What Codex's tool calls change, as their hooks report it: its shell
//! commands and `apply_patch`, whose patch names no lines, are
//! snapshotted before and after.
use serde_json::Value;
use shoal_protocol::changes::ToolChange;

/// The tools whose calls change files, by the names Codex's hook events
/// give them.
const CHANGING: [&str; 6] = [
    "Bash",
    "apply_patch",
    "shell",
    "local_shell",
    "exec_command",
    "unified_exec",
];

/// What the hook `event` of a tool call reports it changes.
pub(super) fn change(event: &Value) -> Option<ToolChange> {
    if !CHANGING.contains(&event["tool_name"].as_str()?) {
        return None;
    }
    let call = event["tool_use_id"].as_str()?.to_owned();
    match event["hook_event_name"].as_str()? {
        "PreToolUse" => Some(ToolChange::Starts { call }),
        "PostToolUse" => Some(ToolChange::Ends { call }),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
