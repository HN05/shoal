use crate::control::Protection;
use crate::control::ProtectionExt;
use crate::control::stops::*;
use crate::executions::ExecutionKind;
use crate::executions::StartedExecution;
use crate::executions::StopRequest;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

/// A tracked idle agent that can resume, in a new workspace.
async fn idle_agent(fixture: &Arc<Fixture>) -> StartedExecution {
    let workspace = fixture.workspace("agent").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    fixture
        .agents
        .track_agent(
            &*fixture.daemon,
            &execution.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
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
async fn recorded(fixture: &Arc<Fixture>, id: &ExecutionId) -> Option<Protection> {
    let id = id.clone();
    fixture
        .agents
        .store
        .run(move |db| get(db, &id))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_protection_stop_is_recorded_until_the_agent_runs_again() {
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    assert!(*execution.stop.borrow());
    let stop = recorded(&fixture, id).await.unwrap();
    assert_eq!(stop.reason, "critical memory pressure");
    assert_eq!(fixture.agents.agent_overload(id).await, Some(stop));
    // The recorded stop refuses another.
    let restart = fixture
        .agents
        .restart_agent(id, true, None)
        .await
        .unwrap_err();
    assert!(restart.to_string().contains("is stopped"), "{restart:#}");

    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, id, None)
            .await
            .unwrap()
    );
    assert!(!*execution.stop.borrow());
    assert_eq!(recorded(&fixture, id).await, None);
}

#[tokio::test]
async fn a_stop_request_cancels_the_recorded_stop() {
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    let connections = fixture.agents.executions().connections().await;
    assert!(
        connections
            .stop(id, &StopRequest::queue_cancel())
            .await
            .unwrap()
    );
    drop(connections);
    // A daemon shutdown leaves the record for the next daemon, which the
    // wrapper tells whether its restore still waits.
    assert!(recorded(&fixture, id).await.is_some());
    assert_eq!(fixture.agents.agent_overload(id).await, None);
}

#[tokio::test]
async fn a_stop_that_loses_leaves_the_winners_handoff() {
    use crate::execution::recovery::{Record, RecordExt};
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    let lost = fixture
        .agents
        .protect(id, &Protection::load("disk space"), None, None)
        .await;
    assert!(lost.is_none());
    let workspace = (fixture.agents.executions())
        .with_agent(id, |agent| agent.workspace_id.clone())
        .await
        .unwrap();
    let path = crate::execution::recovery::record_path(&fixture.agents.paths, &workspace, id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("critical memory pressure")
    );
}

#[tokio::test]
async fn a_resume_after_a_stop_request_leaves_the_agent_stopped() {
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    let connections = fixture.agents.executions().connections().await;
    assert!(
        connections
            .stop(id, &StopRequest::queue_cancel())
            .await
            .unwrap()
    );
    drop(connections);
    assert!(
        !fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, id, None)
            .await
            .unwrap()
    );
    assert!(*execution.stop.borrow());
}

#[tokio::test]
async fn a_load_stop_the_store_cannot_record_still_stops_the_agent() {
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    // The store refuses every stop record, as on a full disk.
    fixture
        .agents
        .store
        .run(|db| {
            db.execute_batch("DROP TABLE agent_stops")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        fixture
            .agents
            .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
            .await
    );
    assert!(*execution.stop.borrow());
    let request = fixture.agents.executions().stop_request(id).await.unwrap();
    assert_eq!(request.save, crate::executions::Save::Everything);
}

#[tokio::test]
async fn a_stop_recorded_beside_a_restart_leaves_the_restart_its_stop() {
    let fixture = Arc::new(Fixture::new().await);
    let execution = idle_agent(&fixture).await;
    let id = &execution.plan.id;
    // A restart signals the wrapper, then a protection stop commits its
    // record before its delivery finds the restart.
    (fixture.agents.executions())
        .with_agent(id, |agent| {
            agent.restarts.current = Some(crate::control::restart::Restart::new(false, None));
        })
        .await;
    let protection = Protection::load("critical memory pressure");
    assert!(
        fixture
            .agents
            .record_agent_stop(id, &protection, None)
            .await
            .unwrap()
    );
    assert_eq!(fixture.agents.agent_overload(id).await, None);
    assert!(fixture.agents.restart_requested(id).await);
}
