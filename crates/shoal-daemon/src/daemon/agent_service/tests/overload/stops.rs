use crate::daemon::overload::Pressure;
use crate::daemon::test_support::manager;
use shoal_ids::execution_ids::ExecutionId;
use shoal_schedule::stopping::{Stop, Work};

/// A demo restarted for load waits for healthy readings before load picks
/// it again, so it cannot keep the agents that load comes from running.
#[tokio::test]
async fn a_restarted_demo_waits_for_recovery_before_load_picks_it_again() {
    let (_root, manager) = manager().await;
    let execution: ExecutionId = "demo".into();
    let demo = Stop {
        space: "space".into(),
        work: Work::Demo(execution.clone()),
    };
    assert!(manager.agents.frees(&*manager, &demo, Pressure::Cpu));
    manager
        .agents
        .overload_recovery()
        .demo_restarted(&execution);
    assert!(!manager.agents.frees(&*manager, &demo, Pressure::Cpu));
    assert!(!manager.agents.frees(&*manager, &demo, Pressure::Memory));
    // Readings of an earlier epoch admit nothing.
    let recovery = &manager.agents.overload_recovery();
    recovery.publish(Some(recovery.epoch() + 1));
    assert!(!manager.agents.frees(&*manager, &demo, Pressure::Cpu));
    recovery.publish(Some(recovery.epoch()));
    assert!(manager.agents.frees(&*manager, &demo, Pressure::Cpu));
}
