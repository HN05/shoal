//! The plan Codex's `update_plan` tool reports to its `PostToolUse` hook,
//! which sends the whole plan each time.
use serde_json::Value;

use crate::client::progress::{PlanStep, ProgressChange, StepState};

/// The plan tool, as a hook matcher.
pub(super) const TOOLS: &str = "update_plan";

/// The change the `PostToolUse` `event` of an `update_plan` call reports.
pub(super) fn change(event: &Value) -> Option<ProgressChange> {
    if event["tool_name"].as_str()? != "update_plan" {
        return None;
    }
    let input = arguments(&event["tool_input"])?;
    let steps = (input["plan"].as_array()?.iter())
        .filter_map(|item| {
            Some(PlanStep {
                name: item["step"].as_str()?.to_owned(),
                state: state(item["status"].as_str()?)?,
                key: None,
            })
        })
        .collect();
    Some(ProgressChange::Steps(steps))
}

/// The call's arguments, which Codex may pass as their JSON text.
fn arguments(input: &Value) -> Option<Value> {
    match input {
        Value::String(text) => serde_json::from_str(text).ok(),
        Value::Object(_) => Some(input.clone()),
        _ => None,
    }
}

fn state(status: &str) -> Option<StepState> {
    match status {
        "pending" => Some(StepState::Pending),
        "in_progress" => Some(StepState::Active),
        "completed" => Some(StepState::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
