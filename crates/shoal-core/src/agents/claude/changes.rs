//! What Claude Code's tool calls change, as their hooks report it: `Edit`,
//! `Write` and `MultiEdit` pass the edit itself, the patch Claude Code
//! made of it, while a `Bash` command and `NotebookEdit`, whose result
//! carries no patch, are snapshotted before and after.
use serde_json::Value;
use shoal_protocol::changes::{Hunk, ToolChange, header_start, split};

/// The tools whose calls are snapshotted, as a hook matcher.
pub(super) const SNAPSHOTTED: &str = "Bash|NotebookEdit";

/// Every tool whose calls change files, as a hook matcher.
pub(super) const CHANGING: &str = "Bash|NotebookEdit|Edit|Write|MultiEdit";

/// What the hook `event` of a tool call reports it changes.
pub(super) fn change(event: &Value) -> Option<ToolChange> {
    let call = || event["tool_use_id"].as_str().map(str::to_owned);
    let tool = event["tool_name"].as_str()?;
    match (event["hook_event_name"].as_str()?, tool) {
        ("PreToolUse", "Bash" | "NotebookEdit") => Some(ToolChange::Starts { call: call()? }),
        ("PostToolUse" | "PostToolUseFailure", "Bash" | "NotebookEdit") => {
            Some(ToolChange::Ends { call: call()? })
        }
        ("PostToolUse", "Edit" | "Write" | "MultiEdit") => edited(event),
        _ => None,
    }
}

/// The edit an edit tool's result reports: the file it wrote, with the
/// hunks of its `structuredPatch`, or every line of a file it created.
fn edited(event: &Value) -> Option<ToolChange> {
    let response = &event["tool_response"];
    let path = (response["filePath"].as_str()).or(event["tool_input"]["file_path"].as_str())?;
    let patch = response["structuredPatch"].as_array()?;
    let hunks = match (response["type"].as_str(), &patch[..]) {
        (Some("create"), []) => vec![Hunk::created(response["content"].as_str()?)],
        (_, patch) => patch.iter().filter_map(hunks).flatten().collect(),
    };
    Some(ToolChange::Edited {
        path: path.into(),
        hunks,
    })
}

/// The hunks of one hunk of a `structuredPatch`, which counts its lines
/// from 1 as a unified diff's header does.
fn hunks(hunk: &Value) -> Option<Vec<Hunk>> {
    let number = |key: &str| {
        hunk[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
    };
    let before = header_start(number("oldStart")?, number("oldLines")?);
    let after = header_start(number("newStart")?, number("newLines")?);
    let lines = hunk["lines"].as_array()?.iter().filter_map(Value::as_str);
    Some(split(before, after, lines))
}

#[cfg(test)]
mod tests;
