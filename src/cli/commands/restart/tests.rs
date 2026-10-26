use super::*;
use crate::{model::AgentStatus, state::WorkspaceState};

fn workspace(name: &str, state: Option<AgentState>) -> Workspace {
    let mut workspace = Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        name.into(),
        WorkspaceState::Ready,
    );
    workspace.agent_state = state.map(|state| AgentStatus { state, since: 0 });
    workspace
}

fn running(workspace: &Workspace, agent: Option<&str>, restorable: bool) -> RunningAgent {
    RunningAgent {
        execution: format!("{}-execution", workspace.name),
        workspace_id: workspace.id.clone(),
        agent: agent.map(str::to_owned),
        restorable,
    }
}

fn restarted(plan: &Plan) -> Vec<&str> {
    plan.restarts
        .iter()
        .map(|restart| restart.workspace.name.as_str())
        .collect()
}

fn skipped(plan: &Plan) -> Vec<(&str, &str)> {
    plan.skipped
        .iter()
        .map(|skipped| (skipped.workspace.as_str(), skipped.reason.as_str()))
        .collect()
}

#[test]
fn idle_agents_restart_first_and_waiting_or_silent_ones_count_as_working() {
    let working = workspace("working", Some(AgentState::Working));
    let waiting = workspace("waiting", Some(AgentState::Waiting));
    let silent = workspace("silent", None);
    let idle = workspace("idle", Some(AgentState::Idle));
    let agents = [&working, &waiting, &silent, &idle]
        .map(|workspace| running(workspace, Some("claude"), true))
        .to_vec();
    let selection = Selection {
        workspaces: vec![working, waiting, silent, idle],
        agents,
        named: false,
    };
    let plan = selection.plan(None);
    assert_eq!(restarted(&plan), ["idle", "working", "waiting", "silent"]);
    assert!(plan.skipped.is_empty());
}

#[test]
fn an_agent_name_selects_its_agents_and_unrestorable_ones_are_skipped() {
    let claude = workspace("claude", None);
    let codex = workspace("codex", None);
    let custom = workspace("custom", None);
    let gone = workspace("gone", None);
    let commands = workspace("commands", None);
    let selection = Selection {
        agents: vec![
            running(&claude, Some("claude"), true),
            running(&codex, Some("codex"), true),
            running(&custom, Some("claude"), false),
            running(&gone, None, false),
        ],
        workspaces: vec![claude, codex, custom, gone, commands],
        named: false,
    };
    let plan = selection.plan(Some("claude"));
    assert_eq!(restarted(&plan), ["claude"]);
    assert_eq!(
        skipped(&plan),
        [
            (
                "custom",
                "claude has no command to resume its session; use shoal stop custom and shoal resume custom"
            ),
            (
                "gone",
                "execution gone-execution is disconnected; use shoal stop gone and shoal resume gone"
            ),
        ]
    );
}

#[test]
fn a_named_workspace_without_the_agent_is_reported() {
    let codex = workspace("codex", None);
    let selection = Selection {
        agents: vec![running(&codex, Some("codex"), true)],
        workspaces: vec![codex],
        named: true,
    };
    let plan = selection.plan(Some("claude"));
    assert!(plan.restarts.is_empty());
    assert_eq!(skipped(&plan), [("codex", "no claude agent runs there")]);
}
