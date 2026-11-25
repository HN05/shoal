use crate::control::{ProtectionExt, Select};
use crate::executions::{ExecutionKind, StartedExecution};
use crate::model::{Protection, Workspace};
use crate::overload::Pressure;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use crate::time::UnixTime;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{executions::TrackedAgent, space_names::SpaceSelector};
use shoal_schedule::stopping::Stop;
use shoal_schedule::stopping::Work;
use std::{sync::Arc, time::Duration};

/// A demo fixture for load waits for healthy readings before load picks
/// it again, so it cannot keep the agents that load comes from running.
#[tokio::test]
async fn a_restarted_demo_waits_for_recovery_before_load_picks_it_again() {
    let fixture = Arc::new(Fixture::new().await);
    let execution: ExecutionId = "demo".into();
    let demo = Stop {
        space: "space".into(),
        work: Work::Demo(execution.clone()),
    };
    assert!(fixture.agents.frees(&*fixture.daemon, &demo, Pressure::Cpu));
    fixture
        .agents
        .overload_recovery()
        .demo_restarted(&execution);
    assert!(!fixture.agents.frees(&*fixture.daemon, &demo, Pressure::Cpu));
    assert!(
        !fixture
            .agents
            .frees(&*fixture.daemon, &demo, Pressure::Memory)
    );
    // Readings of an earlier epoch admit nothing.
    let recovery = &fixture.agents.overload_recovery();
    recovery.publish(Some(recovery.epoch() + 1));
    assert!(!fixture.agents.frees(&*fixture.daemon, &demo, Pressure::Cpu));
    recovery.publish(Some(recovery.epoch()));
    assert!(fixture.agents.frees(&*fixture.daemon, &demo, Pressure::Cpu));
}

/// Memory pressure stops an asleep agent's running lease where no work is
/// left to stop, one at a time; CPU load leaves them.
#[tokio::test]
async fn memory_pressure_stops_asleep_agents_running_leases_one_at_a_time() {
    use std::sync::atomic::Ordering;
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    daemon.asleep_leases.store(2, Ordering::SeqCst);
    let stop = |pressure| fixture.agents.stop_for_overload(daemon, pressure);

    assert!(!stop(Pressure::Cpu).await);
    assert!(stop(Pressure::Memory).await);
    assert_eq!(daemon.asleep_leases.load(Ordering::SeqCst), 1);
    assert!(stop(Pressure::Memory).await);
    assert!(!stop(Pressure::Memory).await);
}

/// A running chat in the fixture's repository, with a heavy chat mode
/// `pair`, and the execution of its agent, which runs in `mode`.
async fn chat_agent(fixture: &Fixture, mode: &str) -> StartedExecution {
    let repo = fixture.repository().await;
    fixture.configure_repository(&repo.id, "[modes.pair]\nkind = 'chat'\n");
    fixture.chat_agent("talk", mode, &repo).await.1
}

/// Load stops a heavy chat's agent, as it stops a workspace's.
#[tokio::test]
async fn load_stops_a_heavy_chats_agent() {
    let fixture = Fixture::new().await;
    let execution = chat_agent(&fixture, "pair").await;

    let stopped = (fixture.agents).stop_for_overload(&*fixture.daemon, Pressure::Memory);

    assert!(stopped.await);
    assert!(*execution.stop.borrow());
}

/// Load never stops a light chat's agent, while a lack of disk space stops
/// it with the others, since any agent may be writing.
#[tokio::test]
async fn load_leaves_a_light_chats_agent_running_while_disk_space_stops_it() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let execution = chat_agent(&fixture, "chat").await;

    assert!(
        !fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert!(!*execution.stop.borrow());
    let protection = Protection::load("low disk space");
    let stopped = (fixture
        .agents
        .stop_agents_for_disk_space(daemon, &protection))
    .await;
    assert_eq!(stopped.len(), 1);
    assert!(*execution.stop.borrow());
}

/// Track an agent of `mode` in `workspace`, reporting `state`.
async fn agent_in(
    fixture: &Fixture,
    workspace: &Workspace,
    mode: &str,
    state: AgentState,
) -> StartedExecution {
    let daemon = &*fixture.daemon;
    let execution = (fixture.agents)
        .begin_execution(
            daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let agent = TrackedAgent {
        mode: mode.into(),
        ..TrackedAgent::named("claude")
    };
    let id = &execution.plan.id;
    (fixture.agents)
        .track_agent(
            daemon,
            id,
            &agent,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    let selected = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .set_agent_state(daemon, &selected, state, Some(id.clone()), None))
    .await
    .unwrap();
    execution
}

/// A coordinator in a workspace, never interacted with, and a child
/// workspace below it.
async fn coordinator_and_child(fixture: &Fixture) -> (Workspace, Workspace) {
    let parent = fixture.workspace("coordinator").await;
    let child = fixture.workspace("child").await;
    fixture.place_below(&child.id, &parent.id).await;
    (parent, child)
}

/// Load stops a child's working agent before its coordinator's, which
/// would go first on its own, being idle.
#[tokio::test]
async fn load_stops_a_coordinator_only_after_the_agents_below_it() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let (parent, child) = coordinator_and_child(&fixture).await;
    let coordinator = agent_in(&fixture, &parent, "worker", AgentState::Idle).await;
    let worker = agent_in(&fixture, &child, "worker", AgentState::Working).await;
    (fixture
        .agents
        .record_interaction(&child.id, UnixTime::now()))
    .await
    .unwrap();

    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert!(*worker.stop.borrow());
    assert!(!*coordinator.stop.borrow());
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert!(*coordinator.stop.borrow());
}

/// Load never stops a light agent for its own sake, but stopping the heavy
/// coordinator above it stops it first.
#[tokio::test]
async fn load_stops_a_light_agent_below_the_heavy_one_it_stops() {
    let fixture = Fixture::new().await;
    let (parent, child) = coordinator_and_child(&fixture).await;
    let coordinator = agent_in(&fixture, &parent, "worker", AgentState::Working).await;
    let light = agent_in(&fixture, &child, "chat", AgentState::Working).await;

    let stopped = (fixture.agents.stop_agents_by_turn(
        &*fixture.daemon,
        &Protection::load("critical memory pressure"),
        Select::One(Pressure::Memory),
        fixture.agents.agent_turns().await.unwrap(),
    ))
    .await;
    let order: Vec<_> = stopped.iter().map(|agent| agent.id.clone()).collect();
    assert_eq!(order, [light.plan.id.clone(), coordinator.plan.id.clone()]);
}

/// An agent recorded below a coordinator whose wrapper has yet to
/// reconnect, as after a daemon restart, keeps the coordinator running.
#[tokio::test]
async fn load_leaves_a_coordinator_running_above_an_agent_it_cannot_reach() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let (parent, child) = coordinator_and_child(&fixture).await;
    let coordinator = agent_in(&fixture, &parent, "worker", AgentState::Idle).await;
    let unreached = (fixture
        .agents
        .begin_agent_execution(daemon, child.id.as_str()))
    .await
    .unwrap();
    // Recorded as the daemon that tracked it left it.
    let id = unreached.plan.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            db.execute("UPDATE executions SET agent='claude' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();

    assert!(
        !fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert!(!*coordinator.stop.borrow());
}
