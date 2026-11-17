use std::{fs, process::Command};

use super::*;
use crate::test_support::{install_test_script, printed};

#[test]
fn state_hooks_report_each_state_and_never_fail() {
    let directory = tempfile::tempdir().unwrap();
    // A stand-in Shoal that records its arguments, then fails.
    let log = directory.path().join("calls");
    let shoal = directory.path().join("shoal it's");
    install_test_script(
        &shoal,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", log.display()),
    );
    let quoted = crate::shell::quote(&[shoal.to_str().unwrap()]);
    let group = state(&quoted, Some("idle_prompt"), AgentState::Waiting);
    assert_eq!(group["matcher"], "idle_prompt");
    let command = group["hooks"][0]["command"].as_str().unwrap();
    let output = Command::new("sh").args(["-c", command]).output().unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.lines().last().unwrap(),
        "internal agent-state waiting"
    );
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
}

#[test]
fn plan_hooks_match_the_plan_tools_and_never_fail() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("calls");
    let shoal = directory.path().join("shoal");
    install_test_script(
        &shoal,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", log.display()),
    );
    let quoted = crate::shell::quote(&[shoal.to_str().unwrap()]);
    let (event, group) = plan(&quoted, crate::ai::IntegrationKind::Codex, "update_plan");
    assert_eq!(
        (event, &group["matcher"]),
        ("PostToolUse", &json!("update_plan"))
    );
    let command = group["hooks"][0]["command"].as_str().unwrap();
    let output = Command::new("sh").args(["-c", command]).output().unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(calls.trim_end(), "internal plan --integration codex");
}

#[test]
fn hooks_run_the_started_path_after_an_upgrade_replaces_its_binary() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shoal");
    fs::write(&path, "old").unwrap();
    let started = crate::installed::Executable::at(path.clone());
    fs::remove_file(&path).unwrap();
    fs::write(&path, "new").unwrap();
    assert_eq!(executable(&started).unwrap(), path);
}

#[test]
fn subagent_hooks_report_starts_stops_and_session_starts_and_never_fail() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("calls");
    let shoal = directory.path().join("shoal");
    install_test_script(
        &shoal,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", log.display()),
    );
    let quoted = crate::shell::quote(&[shoal.to_str().unwrap()]);
    let groups = subagent_reports(&quoted, crate::ai::IntegrationKind::Claude);
    let events: Vec<_> = groups.iter().map(|(event, _)| *event).collect();
    assert_eq!(events, ["SessionStart", "SubagentStart", "SubagentStop"]);
    // A compaction restarts the session mid-turn, leaving its subagents.
    assert_eq!(groups[0].1["matcher"], SESSION_START);
    for (_, group) in &groups {
        let command = group["hooks"][0]["command"].as_str().unwrap();
        let output = Command::new("sh").args(["-c", command]).output().unwrap();
        assert!(output.status.success(), "{}", printed(&output));
    }
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        calls
            .lines()
            .all(|call| call == "internal subagent --integration claude"),
        "{calls}"
    );
}

#[test]
fn subagent_events_name_the_change_to_the_running_subagents() {
    use crate::protocol::SubagentChange;
    let started = json!({"hook_event_name": "SubagentStart", "agent_id": "a1"});
    let stopped = json!({"hook_event_name": "SubagentStop", "agent_id": "a1"});
    let session = json!({"hook_event_name": "SessionStart", "source": "clear"});
    assert_eq!(
        subagent_change(&started),
        Some(SubagentChange::Started {
            agent_id: "a1".into()
        })
    );
    assert_eq!(
        subagent_change(&stopped),
        Some(SubagentChange::Stopped {
            agent_id: "a1".into()
        })
    );
    assert_eq!(
        subagent_change(&session),
        Some(SubagentChange::SessionStarted)
    );
    for event in [
        json!({"hook_event_name": "SubagentStop"}),
        json!({"hook_event_name": "Stop", "agent_id": "a1"}),
    ] {
        assert_eq!(subagent_change(&event), None, "{event}");
    }
}
