use super::*;
use crate::daemon::test_support::manager;

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
    assert!(manager.frees(&demo, Pressure::Cpu));
    manager.overload_recovery.demo_restarted(&execution);
    assert!(!manager.frees(&demo, Pressure::Cpu));
    assert!(!manager.frees(&demo, Pressure::Memory));
    // Readings of an earlier epoch admit nothing.
    let recovery = &manager.overload_recovery;
    recovery.publish(Some(recovery.epoch() + 1));
    assert!(!manager.frees(&demo, Pressure::Cpu));
    recovery.publish(Some(recovery.epoch()));
    assert!(manager.frees(&demo, Pressure::Cpu));
}
