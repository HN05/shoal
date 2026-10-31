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
            &crate::execution::TrackedAgent::named(name),
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
                restarted_by: Vec::new(),
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
                restarted_by: Vec::new(),
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

    let error = manager
        .restart_agent(&custom.plan.id, None)
        .await
        .unwrap_err();
    assert!(format!("{error}").contains("resume_command"), "{error}");
    assert!(!*custom.stop.borrow());
    // A refused restart saves no handoff.
    assert!(!recovery::record_path(&manager.paths, &workspace.id, &custom.plan.id).exists());
    let error = manager
        .restart_agent(&stopping.plan.id, None)
        .await
        .unwrap_err();
    assert_eq!(format!("{error}"), "claude is already stopping");
    assert!(!manager.restart_requested(&stopping.plan.id).await);
    let error = manager.restart_agent("unknown", None).await.unwrap_err();
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
        async move { manager.restart_agent(&id, None).await }
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
        async move { manager.restart_agent(&id, None).await }
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
        async move { manager.restart_agent(&id, None).await }
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
        async move { manager.restart_agent(&id, Some("first".into())).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    // Registered before the agent starts again, which answers it; the
    // restart keeps the run that asked for it first.
    let second = manager
        .request_restart(&id, Some("second".into()))
        .await
        .unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(
        listed[0].restart,
        Some(PendingRestart {
            run: Some("first".into()),
            joined: vec!["second".into()],
        })
    );
    // The agent's start answers both.
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(listed[0].restart, None);
    // The run that joined got its restart too.
    assert_eq!(listed[0].restarted_by, ["first", "second"]);
    // Joining once the restart ended finds the agent running again, and
    // starts no other restart.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*stop.borrow());
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
        async move { manager.restart_agent(&id, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.notify_restore_waits(&id, "PID 7 survives").await;
    restarting.await.unwrap().unwrap_err();
    // Other processes keep it waiting now.
    manager.restart_waits(&id, "PID 8 survives").await;

    let error = manager.restart_agent(&id, None).await.unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert_eq!(
        format!("{error}"),
        "claude restarts once its remaining processes exit: PID 8 survives; \
         shoal stop agents cancels the restart"
    );
    assert!(manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_late_join_answers_as_the_restart_ended() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let error = manager.join_restart(&id, None, None).await.unwrap_err();
    assert_eq!(format!("{error}"), "no restart of claude is in progress");

    let failed = manager.request_restart(&id, None).await.unwrap();
    manager
        .fail_restart(&id, "automatic restore failed".into())
        .await;
    assert!(failed.await.unwrap().is_err());
    let error = manager.join_restart(&id, None, None).await.unwrap_err();
    assert_eq!(format!("{error}"), "automatic restore failed");
    assert!(!manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_late_join_of_a_cancelled_restart_fails() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let cancelled = manager.request_restart(&id, None).await.unwrap();
    manager
        .executions
        .with_agent(&id, |agent| agent.cancel_recovery())
        .await;
    // The request in progress learns it too.
    assert!(cancelled.await.is_err());
    let error = manager.join_restart(&id, None, None).await.unwrap_err();
    assert_eq!(format!("{error}"), "the restart was cancelled");
}

#[tokio::test]
async fn a_request_of_a_run_that_restarted_the_agent_already_leaves_it_running() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarted = manager
        .request_restart(&id, Some("run".into()))
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    restarted.await.unwrap().unwrap();

    // A later request of the same run, such as from another command that
    // continues it, finds the agent restarted and does not stop it.
    manager
        .restart_agent(&id, Some("run".into()))
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*execution.stop.borrow());
    // Another run restarts it again, and a late request of the first run
    // neither joins that restart nor, once it ended, starts another.
    let again = manager
        .request_restart(&id, Some("other".into()))
        .await
        .unwrap();
    assert!(manager.restart_requested(&id).await);
    manager
        .restart_agent(&id, Some("run".into()))
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    again.await.unwrap().unwrap();
    manager
        .restart_agent(&id, Some("run".into()))
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn a_late_join_answers_for_the_restart_of_its_run_while_another_runs() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let first = manager
        .request_restart(&id, Some("first".into()))
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    first.await.unwrap().unwrap();
    // Another command restarts the agent for its own run.
    let second = manager
        .request_restart(&id, Some("second".into()))
        .await
        .unwrap();

    // The first run's late join finds its restart ended well, and does not
    // wait for the second.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
    // The second run's join waits for its own, which then fails.
    let joined = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move {
            manager
                .join_restart(&id, Some("second".into()), Some("second".into()))
                .await
        }
    });
    manager.fail_restart(&id, "cancelled".into()).await;
    assert!(second.await.unwrap().is_err());
    let error = joined.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
    // The first run's outcome stays its own.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_join_counts_the_restart_for_the_run_that_continues() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarted = manager
        .request_restart(&id, Some("b".into()))
        .await
        .unwrap();
    // A run continuing as `a` joins the restart `b` asked for, before or
    // after it ends; either way it counts for `a`.
    let joined = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move {
            manager
                .join_restart(&id, Some("b".into()), Some("a".into()))
                .await
        }
    });
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    restarted.await.unwrap().unwrap();
    joined.await.unwrap().unwrap();
    // Joining it once it ended counts it for that run as well.
    manager
        .join_restart(&id, Some("b".into()), Some("c".into()))
        .await
        .unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(listed[0].restarted_by, ["a", "b", "c"]);
}
