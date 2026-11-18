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

fn names(spaces: &[Space]) -> Vec<&str> {
    spaces.iter().map(|space| space.name.as_str()).collect()
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
