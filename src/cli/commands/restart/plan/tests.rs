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
        restarted_by: Vec::new(),
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

/// An agent of `workspace` the run `restarted_by` restarted, restarting
/// for the run `restarting` when that is given.
fn restarted_in(
    workspace: &Workspace,
    restarted_by: Option<&str>,
    restarting: Option<&str>,
) -> RunningAgent {
    RunningAgent {
        restarted_by: restarted_by.map(Into::into).into_iter().collect(),
        restart: restarting.map(|run| PendingRestart {
            run: Some(run.into()),
        }),
        ..reporting(workspace, Some(AgentState::Working))
    }
}

#[test]
fn a_rerun_continues_the_interrupted_run_and_skips_what_it_restarted() {
    let [pending, again, earlier, other] = ["pending", "again", "earlier", "other"].map(workspace);
    let selection = Selection {
        agents: vec![
            restarted_in(&pending, None, Some("interrupted")),
            // Restarted first by a sequential run, before it asked for pending.
            restarted_in(&again, Some("interrupted"), None),
            restarted_in(&earlier, None, None),
            restarted_in(&other, Some("older"), None),
        ],
        workspaces: vec![pending, again, earlier, other],
        named: false,
    };
    let plan = selection.plan(None);
    assert_eq!(plan.run, "interrupted");
    // The restart in progress comes first, ready although its turn was
    // working, since its agent is already stopped.
    assert_eq!(restarted(&plan), ["pending", "earlier", "other"]);
    assert_eq!(
        plan.restarts[0].joins,
        Some(PendingRestart {
            run: Some("interrupted".into())
        })
    );
    assert!(plan.restarts[0].ready());
    assert!(plan.restarts[1].joins.is_none() && !plan.restarts[1].ready());
    assert_eq!(
        skipped(&plan),
        [("again", "the interrupted run restarted it already")]
    );
}

#[test]
fn without_restarts_in_progress_a_run_restarts_every_agent_under_a_new_name() {
    let recent = workspace("recent");
    let selection = Selection {
        agents: vec![restarted_in(&recent, Some("earlier"), None)],
        workspaces: vec![recent],
        named: false,
    };
    let plan = selection.plan(None);
    assert_eq!(restarted(&plan), ["recent"]);
    assert_ne!(plan.run, "earlier");
}
