use crate::daemon::agents::stops::*;
use crate::daemon::{
    agents::{Protection, ProtectionExt},
    manager::Manager,
};
use crate::daemon::{
    executions::{ExecutionKind, StartedExecution, StopRequest},
    test_support::{manager, repository},
};
use crate::state::AgentState;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// A tracked idle agent that can resume, in a new workspace.
async fn idle_agent(manager: &Manager, root: &tempfile::TempDir) -> StartedExecution {
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .track_agent(
            manager,
            &execution.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .agents
        .set_agent_state(
            manager,
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
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    assert!(*execution.stop.borrow());
    let stop = recorded(&manager, id).await.unwrap();
    assert_eq!(stop.reason, "critical memory pressure");
    assert_eq!(manager.agents.agent_overload(id).await, Some(stop));
    // The recorded stop refuses another.
    let restart = manager
        .agents
        .restart_agent(id, true, None)
        .await
        .unwrap_err();
    assert!(restart.to_string().contains("is stopped"), "{restart:#}");

    assert!(
        manager
            .agents
            .resume_agent_execution(&*manager, id, None)
            .await
            .unwrap()
    );
    assert!(!*execution.stop.borrow());
    assert_eq!(recorded(&manager, id).await, None);
}

#[tokio::test]
async fn a_stop_request_cancels_the_recorded_stop() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    let connections = manager.agents.executions().connections().await;
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
    assert_eq!(manager.agents.agent_overload(id).await, None);
}

#[tokio::test]
async fn a_stop_that_loses_leaves_the_winners_handoff() {
    use crate::execution::recovery::{Record, RecordExt};
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    let lost = manager
        .agents
        .protect(id, &Protection::load("disk space"), None, None)
        .await;
    assert!(lost.is_none());
    let workspace = (manager.agents.executions())
        .with_agent(id, |agent| agent.workspace_id.clone())
        .await
        .unwrap();
    let path = crate::execution::recovery::record_path(&manager.paths, &workspace, id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("critical memory pressure")
    );
}

#[tokio::test]
async fn a_resume_after_a_stop_request_leaves_the_agent_stopped() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    let connections = manager.agents.executions().connections().await;
    assert!(
        connections
            .stop(id, &StopRequest::queue_cancel())
            .await
            .unwrap()
    );
    drop(connections);
    assert!(
        !manager
            .agents
            .resume_agent_execution(&*manager, id, None)
            .await
            .unwrap()
    );
    assert!(*execution.stop.borrow());
}

#[tokio::test]
async fn a_load_stop_the_store_cannot_record_still_stops_the_agent() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    // The store refuses every stop record, as on a full disk.
    manager
        .store
        .run(|db| {
            db.execute_batch("DROP TABLE agent_stops")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    assert!(*execution.stop.borrow());
    let request = manager.agents.executions().stop_request(id).await.unwrap();
    assert_eq!(request.save, crate::daemon::executions::Save::Everything);
}

#[tokio::test]
async fn a_stop_recorded_beside_a_restart_leaves_the_restart_its_stop() {
    let (root, manager) = manager().await;
    let execution = idle_agent(&manager, &root).await;
    let id = &execution.plan.id;
    // A restart signals the wrapper, then a protection stop commits its
    // record before its delivery finds the restart.
    (manager.agents.executions())
        .with_agent(id, |agent| {
            agent.restarts.current =
                Some(crate::daemon::agents::restart::Restart::new(false, None));
        })
        .await;
    let protection = Protection::load("critical memory pressure");
    assert!(
        manager
            .agents
            .record_agent_stop(id, &protection, None)
            .await
            .unwrap()
    );
    assert_eq!(manager.agents.agent_overload(id).await, None);
    assert!(manager.agents.restart_requested(id).await);
}
