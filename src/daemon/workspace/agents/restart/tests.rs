use std::time::Duration;

use crate::{
    daemon::workspace::{ExecutionKind, Manager},
    model::{RunningAgent, Workspace},
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
            },
            RunningAgent {
                execution: gone.plan.id.clone(),
                workspace_id: workspace.id.clone(),
                agent: None,
                restorable: false,
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
    // The restore resumes the agent without the queue, and its start answers.
    assert!(manager.resume_agent_execution(&id).await.unwrap());
    assert!(!*stop.borrow_and_update());
    assert!(manager.restart_requested(&id).await);
    manager.finish_restart(&id, Ok(())).await;
    restarting.await.unwrap().unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(manager.notifications(false, 10).await.unwrap().is_empty());

    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id).await }
    });
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.finish_restart(&id, Err("cancelled".into())).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
}
