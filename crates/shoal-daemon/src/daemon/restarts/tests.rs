use std::time::Duration;

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        test_support::{manager, repository},
        workspace::{ExecutionKind, Manager},
    },
    model::{AgentRestart, RestartOutcome, RestartWait, Workspace},
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

async fn track(
    manager: &Manager,
    workspace: &Workspace,
    id: &ExecutionId,
    name: &str,
    recover: bool,
) {
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

/// The reason the daemon gave for skipping the agent of `workspace`.
async fn skipped(manager: &Manager, workspace: &Workspace) -> String {
    let space = SpaceSelector::id(&workspace.id);
    let outcome = manager.restart_space(None, &space, AgentRestart::default());
    match outcome.await.unwrap() {
        RestartOutcome::Skipped { reason, .. } => reason,
        other => panic!("expected the restart skipped: {other:?}"),
    }
}

#[tokio::test]
async fn a_space_whose_agent_cannot_restart_is_skipped_with_why() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    assert!(skipped(&manager, &workspace).await.contains("no agent"));

    let custom = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &custom.plan.id, "custom", false).await;
    assert!(skipped(&manager, &workspace).await.contains("resume"));
    assert!(!*custom.stop.borrow());

    track(&manager, &workspace, &custom.plan.id, "claude", true).await;
    manager
        .stop_agent_for_overload("critical memory pressure")
        .await;
    let reason = skipped(&manager, &workspace).await;
    assert!(reason.contains("critical memory pressure"), "{reason}");
    assert!(
        reason.contains(&format!("shoal queue start {}", workspace.number)),
        "{reason}"
    );
}

#[tokio::test]
async fn a_space_named_restart_answers_once_its_agent_runs_again() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restart = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), SpaceSelector::id(&workspace.id));
        async move { manager.restart_space(None, &space, restart).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    let RestartOutcome::Restarted { agent, .. } = restarting.await.unwrap().unwrap() else {
        panic!("expected the agent restarted");
    };
    assert_eq!(agent.name(), "claude");
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_what_it_waits_for() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), SpaceSelector::id(&workspace.id));
        async move {
            manager
                .restart_space(None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.notify_restore_waits(&id, "PID 7 survives").await;
    let waits = RestartWait::Processes {
        issue: "PID 7 survives".into(),
    };
    assert_eq!(
        restarting.await.unwrap().unwrap(),
        RestartOutcome::Waits {
            agent: "claude".into(),
            waits,
        }
    );
}
