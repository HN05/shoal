use super::*;
use crate::{
    daemon::workspace::ExecutionKind,
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
