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
