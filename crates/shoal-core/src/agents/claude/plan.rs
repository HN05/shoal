//! The plan Claude Code's plan tools report to their `PostToolUse` hook:
//! `TodoWrite` sends the whole list, while `TaskCreate` and `TaskUpdate`
//! send one task by its ID. A subagent's calls, whose events name its
//! `agent_id`, plan the subagent's own work and change nothing.
use serde_json::Value;

use crate::client::progress::{PlanStep, ProgressChange, StepState};

/// The plan tools, as a hook matcher.
pub(super) const TOOLS: &str = "TodoWrite|TaskCreate|TaskUpdate";

/// The change the `PostToolUse` `event` of a plan tool call reports.
pub(super) fn change(event: &Value) -> Option<ProgressChange> {
    if event.get("agent_id").is_some_and(|agent| !agent.is_null()) {
        return None;
    }
    let (input, response) = (&event["tool_input"], &event["tool_response"]);
    match event["tool_name"].as_str()? {
        "TodoWrite" => todos(input),
        "TaskCreate" => created(input, response),
        "TaskUpdate" => updated(input, response),
        _ => None,
    }
}

/// Every todo `TodoWrite` sends, which replace the plan.
fn todos(input: &Value) -> Option<ProgressChange> {
    let steps = (input["todos"].as_array()?.iter())
        .filter_map(|todo| {
            Some(PlanStep {
                name: todo["content"].as_str()?.to_owned(),
                state: state(todo["status"].as_str()?)?,
                key: None,
            })
        })
        .collect();
    Some(ProgressChange::Steps(steps))
}

/// The task `TaskCreate` created, under the ID its result gives it.
fn created(input: &Value, response: &Value) -> Option<ProgressChange> {
    let task = &response["task"];
    let name = task["subject"].as_str().or(input["subject"].as_str())?;
    Some(ProgressChange::StepCreated(PlanStep {
        name: name.to_owned(),
        state: StepState::Pending,
        key: Some(key(&task["id"])?),
    }))
}

/// The change `TaskUpdate` made to the task it names, unless it failed;
/// a `deleted` status removes the task.
fn updated(input: &Value, response: &Value) -> Option<ProgressChange> {
    if response["success"] == Value::Bool(false) {
        return None;
    }
    let status = input["status"].as_str();
    Some(ProgressChange::StepChanged {
        key: key(&input["taskId"]).or_else(|| key(&response["taskId"]))?,
        name: input["subject"].as_str().map(str::to_owned),
        state: status.and_then(state),
        removed: status == Some("deleted"),
    })
}

fn state(status: &str) -> Option<StepState> {
    match status {
        "pending" => Some(StepState::Pending),
        "in_progress" => Some(StepState::Active),
        "completed" => Some(StepState::Done),
        _ => None,
    }
}

/// A task ID, which Claude Code writes as a string of digits.
fn key(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
