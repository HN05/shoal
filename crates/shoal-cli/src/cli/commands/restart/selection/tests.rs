use super::*;
use crate::{
    model::{Protection, Workspace},
    state::WorkspaceState,
};

fn space(name: &str) -> Space {
    Space::from(Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        crate::model::Head::Branch(name.into()),
        WorkspaceState::Ready,
    ))
}

fn running(space: &Space, agent: Option<&str>) -> RunningAgent {
    RunningAgent {
        execution: format!("{}-execution", space.name).into(),
        space_id: space.id.clone(),
        agent: agent.map(Into::into),
        restorable: true,
        state: None,
        interrupted: false,
        origin: None,
        stopped: None,
        restarted_from: Vec::new(),
    }
}

fn names(spaces: &[Target]) -> Vec<&str> {
    spaces.iter().map(|space| space.name.as_str()).collect()
}

/// The joined machine `name`, connected or not.
fn machine(name: &str, connected: bool) -> Member {
    serde_json::from_value(serde_json::json!({
        "id": name, "kind": "machine", "name": name, "joined_at": 0,
        "last_seen": 0, "connected": connected, "hub": false,
    }))
    .unwrap()
}

/// The space `name` of `machine`, which reports `running` running there.
fn listed(name: &str, running: Option<&str>, machine: &Member) -> (Listed, Member) {
    let listed = serde_json::from_value(serde_json::json!({
        "machine": machine.id, "id": name, "number": 7, "name": name, "kind": "workspace",
        "status": "working", "running": running, "reported_at": 0,
    }));
    (listed.unwrap(), machine.clone())
}

/// What a selection takes of `elsewhere`, other machines' spaces.
fn elsewhere(elsewhere: Vec<(Listed, Member)>, agent: Option<&str>, named: bool) -> Selection {
    let mut selection = select(Vec::new(), &[], None, false);
    let agent = agent.map(Agent::from);
    select_elsewhere(&mut selection, elsewhere, agent.as_ref(), named);
    selection
}

fn skipped_names(selection: &Selection) -> Vec<&str> {
    (selection.skipped.iter())
        .map(|skipped| skipped.space.as_str())
        .collect()
}

#[test]
fn another_machine_s_spaces_are_selected_by_the_agent_it_reports_running() {
    let desk = machine("desk", true);
    let spaces = || {
        vec![
            listed("claude", Some("claude"), &desk),
            listed("codex", Some("codex"), &desk),
            listed("idle", None, &desk),
        ]
    };
    assert_eq!(
        names(&elsewhere(spaces(), None, false).spaces),
        ["claude", "codex"]
    );
    let claude = elsewhere(spaces(), Some("claude"), false);
    assert_eq!(names(&claude.spaces), ["claude"]);
    assert!(claude.skipped.is_empty());
    // Named, each goes to its machine unless it runs another agent there.
    assert_eq!(
        names(&elsewhere(spaces(), None, true).spaces),
        ["claude", "codex", "idle"]
    );
    let named = elsewhere(spaces(), Some("claude"), true);
    assert_eq!(names(&named.spaces), ["claude"]);
    assert_eq!(skipped_names(&named), ["codex", "idle"]);
    assert_eq!(named.skipped[0].machine.as_deref(), Some("desk"));
}

#[test]
fn an_offline_machine_s_spaces_are_skipped_unless_named() {
    let laptop = machine("laptop", false);
    let spaces = || vec![listed("away", Some("claude"), &laptop)];
    let selected = elsewhere(spaces(), None, false);
    assert!(selected.spaces.is_empty());
    assert_eq!(skipped_names(&selected), ["away"]);
    assert!(selected.skipped[0].reason.contains("laptop is offline"));
    // A named one goes to the daemon, which refuses it at once, naming
    // the machine.
    assert_eq!(names(&elsewhere(spaces(), None, true).spaces), ["away"]);
}

#[test]
fn without_names_only_spaces_running_a_matching_agent_shoal_did_not_stop_are_selected() {
    let [claude, codex, idle, stopped, gone] =
        ["claude", "codex", "idle", "stopped", "gone"].map(space);
    let mut protected = running(&stopped, Some("claude"));
    protected.stopped = Some(Protection {
        reason: "memory pressure".into(),
        resumes_when: "memory frees".into(),
    });
    let agents = [
        running(&claude, Some("claude")),
        running(&codex, Some("codex")),
        protected,
        // A wrapper that is gone may have run any agent.
        running(&gone, None),
    ];
    let spaces = vec![claude, codex, idle, stopped, gone];
    let selection = select(spaces, &agents, Some(&"claude".into()), false);
    assert_eq!(names(&selection.spaces), ["claude", "gone"]);
    assert!(selection.skipped.is_empty());
}

#[test]
fn named_spaces_go_to_the_daemon_unless_they_run_another_agent() {
    let [claude, codex, idle] = ["claude", "codex", "idle"].map(space);
    let agents = [
        running(&claude, Some("claude")),
        running(&codex, Some("codex")),
    ];
    let spaces = vec![claude, codex, idle];
    let all = select(spaces.clone(), &agents, None, true);
    // The daemon says why it restarts none in `idle`.
    assert_eq!(names(&all.spaces), ["claude", "codex", "idle"]);
    let only = select(spaces, &agents, Some(&"claude".into()), true);
    assert_eq!(names(&only.spaces), ["claude"]);
    let skipped: Vec<&str> = only
        .skipped
        .iter()
        .map(|skipped| skipped.space.as_str())
        .collect();
    assert_eq!(skipped, ["codex", "idle"]);
}

#[test]
fn the_filter_applies_to_the_agent_the_daemon_restarts() {
    let mixed = space("mixed");
    // A wrapper that is gone beside a connected agent of another name.
    let agents = [running(&mixed, None), running(&mixed, Some("codex"))];
    for named in [false, true] {
        let selection = select(vec![mixed.clone()], &agents, Some(&"claude".into()), named);
        assert!(selection.spaces.is_empty(), "named: {named}");
    }
    let codex = select(vec![mixed.clone()], &agents, Some(&"codex".into()), false);
    assert_eq!(names(&codex.spaces), ["mixed"]);
}
