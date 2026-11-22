use crate::config::overload::Overload;
use crate::executions::ExecutionKind;
use crate::stand_in::Fixture;
use crate::warnings::*;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
async fn only_disk_warnings_wake_an_idle_agent() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("busy").await;
    let agent = fixture
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
            &agent.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    let busy = SpaceSelector::id(&workspace.id);
    // A tracked command runs beside the agent.
    let _command = fixture
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
    let wakes = || async {
        fixture
            .agents
            .unread_messages(&workspace.id)
            .await
            .unwrap()
            .wakes
    };
    let settings = Overload::default();
    let mut warnings = Warnings::default();
    let now = Instant::now();
    warnings
        .warn(
            &fixture.agents,
            now,
            Signal::Memory,
            &memory(&settings, false),
        )
        .await;
    warnings
        .warn(&fixture.agents, now, Signal::Cpu, &cpu(&settings))
        .await;
    assert_eq!(
        fixture
            .agents
            .agent_messages(&*fixture.daemon, &busy)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(!wakes().await);
    // An agent can free disk space without running anything.
    warnings
        .warn(
            &fixture.agents,
            now,
            Signal::Disk,
            &disk(&settings, "1.0 GiB available at /"),
        )
        .await;
    assert!(wakes().await);
}

#[test]
fn warnings_are_single_lines_within_the_message_limit() {
    let settings = Overload::default();
    let path = format!("/{}", "x".repeat(200));
    for message in [
        memory(&settings, false),
        memory(&settings, true),
        cpu(&settings),
        disk(&settings, &format!("3.2 GiB available at {path}")),
    ] {
        crate::validate::message(&message).unwrap();
    }
}
