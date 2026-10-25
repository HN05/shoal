//! Hook groups agents run Shoal through, in the event-and-group shape Claude
//! Code and Codex share: `shoal messages --hook` delivers undelivered agent
//! messages as context, and `shoal internal agent-state` reports the turn
//! state. Integrations choose the events and how the agent receives them.
use serde_json::{Map, Value, json};

use crate::state::AgentState;

/// Agent hook events that receive undelivered agent messages as context:
/// after each tool call, and when the user submits a prompt.
pub const MESSAGE_EVENTS: [&str; 2] = ["PostToolUse", "UserPromptSubmit"];

/// How long agents run a message hook before discarding its output.
const MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a message hook waits for the daemon, leaving the rest of
/// [`MESSAGE_TIMEOUT`] to start and print.
pub const MESSAGE_DAEMON_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The session start sources that leave an agent waiting for its first turn:
/// a restart to compact the context happens mid-turn.
pub const SESSION_START: &str = "startup|resume|clear";

/// This Shoal executable quoted for hook shell text. The hooks inherit the
/// execution's scope and state directory.
pub fn shoal() -> std::io::Result<String> {
    let shoal = crate::fsutil::invoked_executable()?;
    Ok(crate::shell::quote(&[&shoal.to_string_lossy()]))
}

/// The groups that deliver messages, one per [`MESSAGE_EVENTS`] event.
pub fn messages(shoal: &str) -> impl Iterator<Item = (&'static str, Value)> {
    let handler = json!({
        "type": "command",
        "command": format!("{shoal} messages --hook"),
        "timeout": MESSAGE_TIMEOUT.as_secs(),
    });
    MESSAGE_EVENTS
        .into_iter()
        .map(move |event| (event, json!({"hooks": [handler.clone()]})))
}

/// The group that tells a subagent, as it starts, which workspace it works
/// in: it inherits the agent's scope, and under Codex its instructions too.
pub fn subagent(shoal: &str) -> (&'static str, Value) {
    let group = json!({"hooks": [{
        "type": "command",
        "command": format!("{shoal} internal subagent-start"),
        "timeout": 10,
    }]});
    ("SubagentStart", group)
}

/// The group that reports `state`, for events `matcher` selects.
pub fn state(shoal: &str, matcher: Option<&str>, state: AgentState) -> Value {
    let mut group = json!({ "hooks": [{
        "type": "command",
        "command": report(shoal, state),
        "timeout": 10,
    }] });
    if let Some(matcher) = matcher {
        group["matcher"] = matcher.into();
    }
    group
}

/// Shell text reporting `state` that never fails, so a stopped daemon cannot
/// block or interrupt the agent's turn.
fn report(shoal: &str, state: AgentState) -> String {
    format!("{shoal} internal agent-state {state} >/dev/null 2>&1 || true")
}

/// Hook groups collected under their events, in order.
pub fn by_event(groups: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    let mut hooks = Map::new();
    for (event, group) in groups {
        if let Value::Array(groups) = hooks.entry(event).or_insert_with(|| json!([])) {
            groups.push(group);
        }
    }
    hooks
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use super::*;

    #[test]
    fn state_hooks_report_each_state_and_never_fail() {
        let directory = tempfile::tempdir().unwrap();
        // A stand-in Shoal that records its arguments, then fails.
        let log = directory.path().join("calls");
        let shoal = directory.path().join("shoal it's");
        fs::write(
            &shoal,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&shoal, fs::Permissions::from_mode(0o755)).unwrap();
        let quoted = crate::shell::quote(&[shoal.to_str().unwrap()]);
        for (agent_state, spelling) in [
            (AgentState::Working, "working"),
            (AgentState::Waiting, "waiting"),
            (AgentState::Idle, "idle"),
        ] {
            let group = state(&quoted, Some("idle_prompt"), agent_state);
            assert_eq!(group["matcher"], "idle_prompt");
            let command = group["hooks"][0]["command"].as_str().unwrap();
            let output = Command::new("sh").args(["-c", command]).output().unwrap();
            assert!(output.status.success(), "{spelling}: {output:?}");
            let calls = fs::read_to_string(&log).unwrap();
            assert_eq!(
                calls.lines().last().unwrap(),
                format!("internal agent-state {spelling}")
            );
        }
        assert!(
            state(&quoted, None, AgentState::Idle)
                .get("matcher")
                .is_none()
        );
    }

    #[test]
    fn message_hooks_run_shoal_and_collect_by_event() {
        let hooks = by_event(messages("shoal").chain([("Stop", json!({"hooks": []}))]));
        assert_eq!(
            hooks.keys().collect::<Vec<_>>(),
            ["PostToolUse", "Stop", "UserPromptSubmit"]
        );
        for event in MESSAGE_EVENTS {
            let handler = &hooks[event][0]["hooks"][0];
            assert_eq!(handler["command"], "shoal messages --hook");
            assert_eq!(handler["type"], "command");
        }
    }
}
