use super::*;
use crate::{
    daemon::workspace::ExecutionKind,
    state::AgentState,
    test_support::{manager, repository},
};

#[tokio::test]
async fn running_agents_are_warned_once_per_signal_until_the_repeat_interval() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut agents = Vec::new();
    for name in ["first", "second", "idle"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        if name != "idle" {
            let execution = manager
                .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
                .await
                .unwrap();
            // The execution's stop receiver keeps the agent connected.
            agents.push((workspace, execution));
        }
    }
    let track = |index: usize| {
        let (workspace, execution) = &agents[index];
        manager.track_agent(
            &execution.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
    };
    let settings = Overload::default();
    let count = |name: &'static str| {
        let manager = manager.clone();
        async move { manager.agent_messages(name).await.unwrap().len() }
    };
    let mut warnings = Warnings::default();
    let start = Instant::now();
    let later = start + Duration::from_secs(60);
    track(0).await;
    warnings
        .warn(&manager, start, Signal::Memory, &memory(&settings))
        .await;
    let delivered = manager.agent_messages("first").await.unwrap();
    manager
        .mark_agent_messages_delivered("first", vec![delivered[0].id])
        .await
        .unwrap();
    // Within the interval, only an agent started since is warned again,
    // and each signal has its own interval.
    track(1).await;
    warnings
        .warn(&manager, later, Signal::Memory, &memory(&settings))
        .await;
    warnings
        .warn(
            &manager,
            later,
            Signal::Disk,
            &disk(&settings, "1.0 GiB available at /"),
        )
        .await;
    assert_eq!(count("first").await, 1);
    assert_eq!(count("second").await, 2);
    assert_eq!(count("idle").await, 0);
    // After the interval, a persisting condition warns again.
    let repeat = Duration::from_secs(settings.warning.repeat_minutes * 60);
    warnings
        .warn(&manager, start + repeat, Signal::Memory, &memory(&settings))
        .await;
    assert_eq!(count("first").await, 2);
}

#[tokio::test]
async fn agents_with_nothing_running_are_warned_without_waking() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut executions = Vec::new();
    for (name, turn) in [
        ("resting", AgentState::Idle),
        ("working", AgentState::Working),
        ("busy", AgentState::Idle),
    ] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        let id = execution.plan.id.clone();
        manager
            .track_agent(&id, "claude", &workspace.id, name, false, Duration::ZERO)
            .await;
        manager
            .set_agent_state(name, turn, Some(id), None)
            .await
            .unwrap();
        executions.push(execution);
    }
    // A tracked command runs beside the busy workspace's idle agent.
    let busy = manager.workspace("busy").await.unwrap();
    executions.push(
        manager
            .begin_execution(&busy.id, None, ExecutionKind::Command, None)
            .await
            .unwrap(),
    );
    let wakes = |name: &'static str| {
        let manager = manager.clone();
        async move { manager.unread_agent_messages(name).await.unwrap().wakes }
    };
    let settings = Overload::default();
    let mut warnings = Warnings::default();
    let now = Instant::now();
    warnings
        .warn(&manager, now, Signal::Memory, &memory(&settings))
        .await;
    assert_eq!(manager.agent_messages("resting").await.unwrap().len(), 1);
    assert!(!wakes("resting").await);
    // A working agent hears it after its next tool call, and a turn that
    // ends without one leaves nothing to stop.
    assert!(!wakes("working").await);
    assert!(wakes("busy").await);
    // An agent can free disk space without running anything.
    let resting = manager.agent_messages("resting").await.unwrap();
    manager
        .mark_agent_messages_delivered("resting", vec![resting[0].id])
        .await
        .unwrap();
    warnings
        .warn(
            &manager,
            now,
            Signal::Disk,
            &disk(&settings, "1.0 GiB available at /"),
        )
        .await;
    assert!(wakes("resting").await);
}

#[test]
fn warnings_are_single_lines_within_the_message_limit() {
    let settings = Overload::default();
    let path = format!("/{}", "x".repeat(200));
    for message in [
        memory(&settings),
        cpu(&settings),
        disk(&settings, &format!("3.2 GiB available at {path}")),
    ] {
        crate::validate::message(&message).unwrap();
    }
}
