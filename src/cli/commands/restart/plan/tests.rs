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
        Some(name.into()),
        WorkspaceState::Ready,
    )
}

fn spaces<const N: usize>(workspaces: [Workspace; N]) -> Vec<PresentSpace> {
    workspaces.map(PresentSpace::Workspace).into()
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
        stopped: None,
        restarted_from: Vec::new(),
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
        .map(|restart| restart.space.name())
        .collect()
}

fn skipped(plan: &Plan) -> Vec<&str> {
    plan.skipped
        .iter()
        .map(|skipped| skipped.space.as_str())
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
        spaces: spaces([working, waiting, silent, idle]),
        agents,
        named: false,
    };
    let plan = selection.plan(None, false);
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
        spaces: spaces([claude, codex, custom, gone, commands]),
        named: false,
    };
    let plan = selection.plan(Some("claude"), false);
    assert_eq!(restarted(&plan), ["claude"]);
    assert_eq!(skipped(&plan), ["custom", "gone"]);
}

#[test]
fn a_named_workspace_without_the_agent_is_reported() {
    let codex = workspace("codex");
    let selection = Selection {
        agents: vec![running(&codex, Some("codex"), true)],
        spaces: spaces([codex]),
        named: true,
    };
    let plan = selection.plan(Some("claude"), false);
    assert!(plan.restarts.is_empty());
    assert_eq!(skipped(&plan), ["codex"]);
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
        spaces: spaces([shared_state]),
        named: false,
    };
    let plan = selection.plan(None, false);
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
            joined: Vec::new(),
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
        spaces: spaces([pending, again, earlier, other]),
        named: false,
    };
    let plan = selection.plan(None, false);
    assert_eq!(plan.run, "interrupted");
    // The restart in progress comes first, ready although its turn was
    // working, since its agent is already stopped.
    assert_eq!(restarted(&plan), ["pending", "earlier", "other"]);
    assert_eq!(
        plan.restarts[0].joins,
        Some(PendingRestart {
            run: Some("interrupted".into()),
            joined: Vec::new(),
        })
    );
    assert!(plan.restarts[0].ready());
    assert!(plan.restarts[1].joins.is_none() && !plan.restarts[1].ready());
    assert_eq!(skipped(&plan), ["again"]);
}

#[test]
fn without_restarts_in_progress_a_run_restarts_every_agent_under_a_new_name() {
    let recent = workspace("recent");
    let selection = Selection {
        agents: vec![restarted_in(&recent, Some("earlier"), None)],
        spaces: spaces([recent]),
        named: false,
    };
    let plan = selection.plan(None, false);
    assert_eq!(restarted(&plan), ["recent"]);
    assert_ne!(plan.run, "earlier");
}

#[test]
fn a_rerun_skips_what_any_run_of_a_pending_restart_restarted() {
    let [pending, done] = ["pending", "done"].map(workspace);
    // A command continuing `a` joined `b`'s restart of pending and
    // restarted done under `a` before it was interrupted.
    let joined = RunningAgent {
        restart: Some(PendingRestart {
            run: Some("b".into()),
            joined: vec!["a".into()],
        }),
        ..reporting(&pending, Some(AgentState::Working))
    };
    let selection = Selection {
        agents: vec![joined, restarted_in(&done, Some("a"), None)],
        spaces: spaces([pending, done]),
        named: false,
    };
    let plan = selection.plan(None, false);
    assert_eq!(restarted(&plan), ["pending"]);
    assert_eq!(skipped(&plan), ["done"]);
}

/// The chat `name`, whose ID is its name.
fn chat(name: &str) -> PresentSpace {
    PresentSpace::Chat(
        serde_json::from_value(serde_json::json!({
            "id": name,
            "repository_id": "repository",
            "name": name,
            "agent": "claude",
            "pid": 1,
            "started_at": 0,
        }))
        .unwrap(),
    )
}

/// The agent of the chat `name`, which can resume its session when
/// `restorable`.
fn chat_agent(name: &str, restorable: bool) -> RunningAgent {
    RunningAgent {
        execution: name.into(),
        workspace_id: name.into(),
        agent: Some("claude".into()),
        restorable,
        state: None,
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: None,
        stopped: None,
        restarted_from: Vec::new(),
    }
}

#[test]
fn chats_restart_beside_workspace_agents_but_never_in_place() {
    let api = workspace("api");
    let agents = vec![
        reporting(&api, Some(AgentState::Idle)),
        chat_agent("talk", true),
        chat_agent("quiet", false),
    ];
    let selection = || Selection {
        spaces: vec![
            PresentSpace::Workspace(api.clone()),
            chat("talk"),
            chat("quiet"),
        ],
        agents: agents.clone(),
        named: false,
    };

    let plan = selection().plan(None, false);
    assert_eq!(restarted(&plan), ["api", "talk"]);
    assert_eq!(skipped(&plan), ["quiet"]);

    let plan = selection().plan(None, true);
    assert_eq!(restarted(&plan), ["api"]);
    assert_eq!(skipped(&plan), ["talk", "quiet"]);
}
