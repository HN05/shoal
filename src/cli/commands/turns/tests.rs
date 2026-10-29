use super::*;
use crate::{model::Workspace, sessions::HostName};

fn agent(origin: Option<Origin>) -> RunningAgent {
    RunningAgent {
        execution: "e1".into(),
        workspace_id: "w1".into(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin,
        restarted_by: None,
        restart: None,
    }
}

fn workspace() -> Workspace {
    serde_json::from_value(json!({
        "id": "w1",
        "repository_id": "r1",
        "name": "fix",
        "path": "/work/fix",
        "branch": "fix",
        "state": "ready",
    }))
    .unwrap()
}

#[test]
fn only_a_working_or_waiting_agent_runs_a_turn() {
    assert!(runs_turn(Some(AgentState::Working)));
    assert!(runs_turn(Some(AgentState::Waiting)));
    assert!(!runs_turn(Some(AgentState::Idle)));
    // A daemon restart forgets the turn until the agent reports again.
    assert!(!runs_turn(None));
}

#[test]
fn only_an_idle_agent_continues_the_turn_an_interrupt_ended() {
    let interrupted = RunningAgent {
        state: Some(AgentState::Idle),
        interrupted: true,
        ..agent(None)
    };
    assert!(idles_interrupted(&interrupted));
    assert!(!idles_interrupted(&RunningAgent {
        interrupted: false,
        ..interrupted.clone()
    }));
    // A turn it started since then runs.
    assert!(!idles_interrupted(&RunningAgent {
        state: Some(AgentState::Working),
        ..interrupted
    }));
}

#[test]
fn an_agent_outside_any_terminal_host_takes_no_keys() {
    let workspace = workspace();
    let error = Target::of(&workspace, agent(None)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "claude in fix runs in no terminal session Shoal can type into"
    );
    let placed = agent(Some(Origin::created(HostName::Zmx, "fix".into())));
    let target = Target::of(&workspace, placed).unwrap();
    assert_eq!(
        (target.described(), target.place.id()),
        ("claude in fix".to_owned(), "fix")
    );
    assert_eq!(
        serde_json::to_value(&target).unwrap(),
        json!({"workspace": "fix", "agent": "claude"})
    );
}
