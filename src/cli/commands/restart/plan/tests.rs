use super::*;
use crate::{
    model::{AgentStatus, PendingRestart},
    state::WorkspaceState,
};

fn workspace(name: &str) -> Workspace {
    Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        name.into(),
        WorkspaceState::Ready,
    )
}

fn running(workspace: &Workspace, agent: Option<&str>, restorable: bool) -> RunningAgent {
    RunningAgent {
        execution: format!("{}-execution", workspace.name),
        workspace_id: workspace.id.clone(),
        agent: agent.map(str::to_owned),
        restorable,
        state: None,
        interrupted: false,
        origin: None,
        started_ms_ago: Some(0),
        restart: None,
    }
}

fn reporting(workspace: &Workspace, state: Option<AgentState>) -> RunningAgent {
    RunningAgent {
        state,
        ..running(workspace, Some("claude"), true)
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
    let working = workspace("working");
    let waiting = workspace("waiting");
    let silent = workspace("silent");
    let idle = workspace("idle");
    let agents = vec![
        reporting(&working, Some(AgentState::Working)),
        reporting(&waiting, Some(AgentState::Waiting)),
        reporting(&silent, None),
        reporting(&idle, Some(AgentState::Idle)),
    ];
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
    let claude = workspace("claude");
    let codex = workspace("codex");
    let custom = workspace("custom");
    let gone = workspace("gone");
    let commands = workspace("commands");
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
    let codex = workspace("codex");
    let selection = Selection {
        agents: vec![running(&codex, Some("codex"), true)],
        workspaces: vec![codex],
        named: true,
    };
    let plan = selection.plan(Some("claude"));
    assert!(plan.restarts.is_empty());
    assert_eq!(skipped(&plan), [("codex", "no claude agent runs there")]);
}

#[test]
fn only_an_agents_own_report_makes_it_idle() {
    let shared = workspace("shared");
    // The workspace's latest report came from the other agent.
    let mut shared_state = shared.clone();
    shared_state.agent_state = Some(AgentStatus {
        state: AgentState::Idle,
        since: 0,
        prompt_cache_expires_at: None,
    });
    let working = RunningAgent {
        execution: "working".into(),
        ..reporting(&shared, None)
    };
    let idle = RunningAgent {
        execution: "idle".into(),
        ..reporting(&shared, Some(AgentState::Idle))
    };
    let selection = Selection {
        agents: vec![working, idle],
        workspaces: vec![shared_state],
        named: false,
    };
    let plan = selection.plan(None);
    let order: Vec<_> = plan
        .restarts
        .iter()
        .map(|restart| (restart.agent.execution.as_str(), restart.ready()))
        .collect();
    assert_eq!(order, [("idle", true), ("working", false)]);
}

/// An agent of `workspace` that started `started_ms_ago`, restarting since
/// `requested_ms_ago` when that is given.
fn started(
    workspace: &Workspace,
    started_ms_ago: u64,
    requested_ms_ago: Option<u64>,
) -> RunningAgent {
    RunningAgent {
        started_ms_ago: Some(started_ms_ago),
        restart: requested_ms_ago.map(|requested_ms_ago| PendingRestart { requested_ms_ago }),
        ..reporting(workspace, Some(AgentState::Working))
    }
}

#[test]
fn a_rerun_waits_for_restarts_in_progress_and_skips_agents_started_since() {
    let [pending, again, earlier, launched] =
        ["pending", "again", "earlier", "launched"].map(workspace);
    let selection = Selection {
        agents: vec![
            started(&pending, 90_000, Some(60_000)),
            started(&again, 30_000, None),
            started(&earlier, 120_000, None),
            started(&launched, 5_000, None),
        ],
        workspaces: vec![pending, again, earlier, launched],
        named: false,
    };
    let plan = selection.plan(None);
    // The restart in progress comes first, ready although its turn was
    // working, since its agent is already stopped.
    assert_eq!(restarted(&plan), ["pending", "earlier"]);
    assert!(plan.restarts[0].joins && plan.restarts[0].ready());
    assert!(!plan.restarts[1].joins && !plan.restarts[1].ready());
    let reason = "it started after the restarts in progress were requested";
    assert_eq!(skipped(&plan), [("again", reason), ("launched", reason)]);
}

#[test]
fn without_restarts_in_progress_recent_agents_restart_too() {
    let recent = workspace("recent");
    let selection = Selection {
        agents: vec![started(&recent, 1_000, None)],
        workspaces: vec![recent],
        named: false,
    };
    assert_eq!(restarted(&selection.plan(None)), ["recent"]);
}
