use std::time::Duration;

use crate::{
    daemon::workspace::{ExecutionKind, Manager, RestartWaits},
    execution::recovery,
    model::{PendingRestart, RunningAgent, Workspace},
    state::AgentState,
    test_support::{manager, repository},
};

async fn workspace(root: &std::path::Path, manager: &Manager) -> Workspace {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agents".into(), None, None, None)
        .await
        .unwrap()
}

async fn track(manager: &Manager, workspace: &Workspace, id: &str, name: &str, recover: bool) {
    manager
        .track_agent(
            id,
            name,
            &workspace.id,
            &workspace.name,
            recover,
            Duration::ZERO,
        )
        .await;
}

#[tokio::test]
async fn agents_list_connected_agents_and_executions_whose_wrapper_is_gone() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let command = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let agent = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let gone = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &agent.plan.id, "claude", true).await;
    manager.detach_execution(&gone.plan.id).await;
    // A report from the agent is its own state, unlike one from a command.
    manager
        .set_agent_state(
            &workspace.id,
            AgentState::Idle,
            Some(agent.plan.id.clone()),
            None,
        )
        .await
        .unwrap();

    let mut agents = manager.running_agents().await.unwrap();
    agents.sort_by_key(|agent| agent.agent.is_none());
    // A connected agent says how long it has run.
    let started = agents[0].started_ms_ago;
    assert!(started.is_some());
    assert_eq!(
        agents,
        [
            RunningAgent {
                execution: agent.plan.id.clone(),
                workspace_id: workspace.id.clone(),
                agent: Some("claude".into()),
                restorable: true,
                state: Some(AgentState::Idle),
                interrupted: false,
                origin: None,
                started_ms_ago: started,
                restart: None,
            },
            RunningAgent {
                execution: gone.plan.id.clone(),
                workspace_id: workspace.id.clone(),
                agent: None,
                restorable: false,
                state: None,
                interrupted: false,
                origin: None,
                started_ms_ago: None,
                restart: None,
            },
        ]
    );
    // A connected command is not an agent to restart.
    assert!(
        agents
            .iter()
            .all(|agent| agent.execution != command.plan.id)
    );
}

#[tokio::test]
async fn agents_are_listed_by_the_names_agent_selection_takes() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    // Happy records its agents as `happy codex`; `--agent` spells it `happy-codex`.
    track(
        &manager,
        &workspace,
        &execution.plan.id,
        "happy codex",
        true,
    )
    .await;
    let agents = manager.running_agents().await.unwrap();
    assert_eq!(agents[0].agent.as_deref(), Some("happy-codex"));
}

#[tokio::test]
async fn each_agent_keeps_its_own_turn_report_in_a_shared_workspace() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let mut ids = Vec::new();
    for name in ["claude", "codex"] {
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        track(&manager, &workspace, &execution.plan.id, name, true).await;
        ids.push(execution.plan.id);
    }
    // The workspace keeps only the later report; each agent keeps its own.
    for (id, state) in ids.iter().zip([AgentState::Idle, AgentState::Working]) {
        manager
            .set_agent_state(&workspace.id, state, Some(id.clone()), None)
            .await
            .unwrap();
    }
    let states: std::collections::HashMap<_, _> = manager
        .running_agents()
        .await
        .unwrap()
        .into_iter()
        .map(|agent| (agent.execution, agent.state))
        .collect();
    assert_eq!(states[&ids[0]], Some(AgentState::Idle));
    assert_eq!(states[&ids[1]], Some(AgentState::Working));
    let status = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(
        status.workspace.agent_state.map(|status| status.state),
        Some(AgentState::Working)
    );
}

#[tokio::test]
async fn restart_refuses_agents_it_cannot_restore_without_stopping_them() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let custom = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let stopping = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &custom.plan.id, "custom", false).await;
    track(&manager, &workspace, &stopping.plan.id, "claude", true).await;
    manager
        .stop_agent_for_overload("critical memory pressure")
        .await;
    assert!(*stopping.stop.borrow());

    let error = manager.restart_agent(&custom.plan.id).await.unwrap_err();
    assert!(format!("{error}").contains("[agent_resume]"), "{error}");
    assert!(!*custom.stop.borrow());
    // A refused restart saves no handoff.
    assert!(!recovery::record_path(&manager.paths, &workspace.id, &custom.plan.id).exists());
    let error = manager.restart_agent(&stopping.plan.id).await.unwrap_err();
    assert_eq!(format!("{error}"), "claude is already stopping");
    assert!(!manager.restart_requested(&stopping.plan.id).await);
    let error = manager.restart_agent("unknown").await.unwrap_err();
    assert_eq!(
        format!("{error}"),
        "no connected agent runs execution unknown"
    );
}

#[tokio::test]
async fn a_restart_answers_once_the_agent_runs_again_or_with_why_not() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;

    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.restart_requested(&id).await);
    // The handoff is saved before the stop reaches the wrapper.
    let handoff =
        recovery::Record::load(&recovery::record_path(&manager.paths, &workspace.id, &id))
            .unwrap()
            .unwrap();
    assert_eq!(handoff.stage, Some(recovery::Stage::Stopping));
    assert_eq!(
        handoff.stop_reason.as_deref(),
        Some("restarted by shoal restart")
    );
    // A first start that registers only now is not the restarted agent, so
    // the wrapper still receives the restart's stop.
    manager.agent_started(&id).await;
    assert!(manager.restart_requested(&id).await);
    // The restore resumes the agent without the queue, and its start answers.
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    assert!(!*stop.borrow_and_update());
    assert!(manager.restart_requested(&id).await);
    manager.agent_started(&id).await;
    restarting.await.unwrap().unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(manager.notifications(false, 10).await.unwrap().is_empty());

    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.fail_restart(&id, "cancelled".into()).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_at_once_and_still_resumes() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();

    let issue = "unreadable live process environments (PID 7)";
    manager.notify_restore_waits(&id, issue).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert_eq!(
        format!("{error}"),
        format!(
            "claude restarts once its remaining processes exit: {issue}; shoal stop agents cancels the restart"
        )
    );
    // The restart goes on, and its start no longer has a request to answer.
    assert!(manager.restart_requested(&id).await);
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    assert!(!manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_second_request_joins_the_restart_in_progress() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let first = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    // Registered before the agent starts again, which answers it.
    let second = manager.request_restart(&id).await.unwrap();
    // The listing shows the restart, requested since the agent started.
    let listed = manager.running_agents().await.unwrap();
    let Some(PendingRestart { requested_ms_ago }) = listed[0].restart else {
        panic!("expected a pending restart: {listed:?}");
    };
    assert!(requested_ms_ago <= listed[0].started_ms_ago.unwrap());
    // The agent's start answers both.
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(manager.running_agents().await.unwrap()[0].restart, None);
}

#[tokio::test]
async fn a_request_joining_a_waiting_restart_is_answered_with_what_it_waits_for_now() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.notify_restore_waits(&id, "PID 7 survives").await;
    restarting.await.unwrap().unwrap_err();
    // Other processes keep it waiting now.
    manager.restart_waits(&id, "PID 8 survives").await;

    let error = manager.restart_agent(&id).await.unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert_eq!(
        format!("{error}"),
        "claude restarts once its remaining processes exit: PID 8 survives; \
         shoal stop agents cancels the restart"
    );
    assert!(manager.restart_requested(&id).await);
}
