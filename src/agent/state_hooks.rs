//! Hook groups that report an agent's turn state with `shoal internal
//! agent-state`. In Claude Code a finished tool call, failed or not, means a
//! permission prompt or question was answered. No hook follows an interrupted
//! turn, so Claude Code's notice that its prompt has waited for input reports
//! the turn finished. Codex has no such notice, so an interrupted Codex turn
//! stays working until its next turn ends.
use serde_json::{Value, json};

use crate::state::AgentState;

/// Each Claude Code hook event with the group that reports its state;
/// `shoal` is the quoted Shoal executable.
pub(super) fn claude(shoal: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("UserPromptSubmit", group(shoal, None, AgentState::Working)),
        ("PostToolUse", group(shoal, None, AgentState::Working)),
        (
            "PostToolUseFailure",
            group(shoal, None, AgentState::Working),
        ),
        (
            "Notification",
            group(
                shoal,
                Some("permission_prompt|elicitation_dialog"),
                AgentState::Waiting,
            ),
        ),
        (
            "Notification",
            group(shoal, Some("idle_prompt"), AgentState::Idle),
        ),
        ("Stop", group(shoal, None, AgentState::Idle)),
        ("StopFailure", group(shoal, None, AgentState::Idle)),
    ]
}

/// Each Codex hook event with the group that reports its state.
pub(super) fn codex(shoal: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("UserPromptSubmit", group(shoal, None, AgentState::Working)),
        ("PostToolUse", group(shoal, None, AgentState::Working)),
        ("Stop", group(shoal, None, AgentState::Idle)),
    ]
}

fn group(shoal: &str, matcher: Option<&str>, state: AgentState) -> Value {
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use super::*;

    #[test]
    fn hooks_report_each_state_and_never_fail() {
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
        let report = |groups: Vec<(&'static str, Value)>| -> Vec<_> {
            groups
                .into_iter()
                .map(|(event, group)| {
                    let command = group["hooks"][0]["command"].as_str().unwrap();
                    let output = Command::new("sh").args(["-c", command]).output().unwrap();
                    assert!(output.status.success(), "{event}: {output:?}");
                    let calls = fs::read_to_string(&log).unwrap();
                    let state = calls.lines().last().unwrap().to_owned();
                    (event, group["matcher"].as_str().map(str::to_owned), state)
                })
                .collect()
        };
        let expected = |event, matcher: Option<&str>, state: &str| {
            (
                event,
                matcher.map(str::to_owned),
                format!("internal agent-state {state}"),
            )
        };
        assert_eq!(
            report(claude(&quoted)),
            [
                expected("UserPromptSubmit", None, "working"),
                expected("PostToolUse", None, "working"),
                expected("PostToolUseFailure", None, "working"),
                expected(
                    "Notification",
                    Some("permission_prompt|elicitation_dialog"),
                    "waiting"
                ),
                expected("Notification", Some("idle_prompt"), "idle"),
                expected("Stop", None, "idle"),
                expected("StopFailure", None, "idle"),
            ]
        );
        assert_eq!(
            report(codex(&quoted)),
            [
                expected("UserPromptSubmit", None, "working"),
                expected("PostToolUse", None, "working"),
                expected("Stop", None, "idle"),
            ]
        );
    }
}
