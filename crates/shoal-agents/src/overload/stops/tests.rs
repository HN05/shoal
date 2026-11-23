use crate::overload::Pressure;
use crate::stand_in::Fixture;
use shoal_ids::execution_ids::ExecutionId;
use shoal_schedule::stopping::Stop;
use shoal_schedule::stopping::Work;
use std::sync::Arc;

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
