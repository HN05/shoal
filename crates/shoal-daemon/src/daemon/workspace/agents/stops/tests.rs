use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    test_support::{manager, repository},
    workspace::{ExecutionKind, StartedExecution, StopRequest},
};
use crate::state::AgentState;

/// A tracked idle agent that can resume, in a new workspace.
async fn idle_agent(manager: &Manager, root: &tempfile::TempDir) -> StartedExecution {
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &execution.plan.id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .set_agent_state(
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some(execution.plan.id.clone()),
            None,
        )
        .await
        .unwrap();
    execution
}

/// The stop the store records for execution `id`.
async fn recorded(manager: &Manager, id: &ExecutionId) -> Option<Protection> {
    let id = id.clone();
    manager.store.run(move |db| get(db, &id)).await.unwrap()
}

#[tokio::test]
async fn a_protection_stop_is_recorded_until_the_agent_runs_again() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(manager.stop_agent_for_overload("memory pressure").await);
    assert!(*execution.stop.borrow());
    let stop = recorded(&manager, id).await.unwrap();
    assert_eq!(stop.reason, "memory pressure");
    assert_eq!(manager.agent_overload(id).await, Some(stop));
    // The recorded stop refuses another.
    let restart = manager
        .restart_agent(id, None, true, None, None)
        .await
        .unwrap_err();
    assert!(restart.to_string().contains("is stopped"), "{restart:#}");

    assert!(manager.resume_agent_execution(id, None).await.unwrap());
    assert!(!*execution.stop.borrow());
    assert_eq!(recorded(&manager, id).await, None);
}

#[tokio::test]
async fn a_stop_request_cancels_the_recorded_stop() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(manager.stop_agent_for_overload("memory pressure").await);
    let connections = manager.executions.connections().await;
    assert!(
        connections
            .stop(id, &StopRequest::queue_cancel())
            .await
            .unwrap()
    );
    drop(connections);
    // A daemon shutdown leaves the record for the next daemon, which the
    // wrapper tells whether its restore still waits.
    assert!(recorded(&manager, id).await.is_some());
    assert_eq!(manager.agent_overload(id).await, None);
}

#[tokio::test]
async fn a_resume_after_a_stop_request_leaves_the_agent_stopped() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(manager.stop_agent_for_overload("memory pressure").await);
    let connections = manager.executions.connections().await;
    assert!(
        connections
            .stop(id, &StopRequest::queue_cancel())
            .await
            .unwrap()
    );
    drop(connections);
    assert!(!manager.resume_agent_execution(id, None).await.unwrap());
    assert!(*execution.stop.borrow());
}
