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
fn an_agent_outside_any_terminal_host_takes_no_keys() {
    let workspace = workspace();
    let detached = agent(None);
    let error = Typed::of(&workspace, &detached).unwrap_err();
    assert_eq!(
        error.to_string(),
        "claude in fix runs in no terminal session Shoal can type into"
    );
    let placed = agent(Some(Origin::created(HostName::Zmx, "fix".into())));
    let typed = Typed::of(&workspace, &placed).unwrap();
    assert_eq!((typed.name, typed.place.id()), ("claude", "fix"));
}
