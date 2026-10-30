use std::{fs, process::Command};

use super::*;
use crate::test_support::install_test_script;

#[test]
fn state_hooks_report_each_state_and_never_fail() {
    let directory = tempfile::tempdir().unwrap();
    // A stand-in Shoal that records its arguments, then fails.
    let log = directory.path().join("calls");
    let shoal = directory.path().join("shoal it's");
    install_test_script(
        &shoal,
        &format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", log.display()),
    );
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
