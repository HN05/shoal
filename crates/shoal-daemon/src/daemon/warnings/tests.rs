use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    test_support::{manager, repository},
    workspace::ExecutionKind,
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
                .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
                .await
                .unwrap();
            // The execution's stop receiver keeps the agent connected.
            agents.push((workspace, execution));
        }
    }
    let claude = crate::execution::TrackedAgent::named("claude");
    let track = |index: usize| {
        let (workspace, execution) = &agents[index];
        manager.track_agent(
            &execution.plan.id,
            &claude,
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
    };
    let settings = Overload::default();
    let count = |name: &'static str| {
        let manager = manager.clone();
        async move {
            manager
                .agent_messages(&manager.named(name).await)
                .await
                .unwrap()
                .len()
        }
    };
    let mut warnings = Warnings::default();
    let start = Instant::now();
    let later = start + Duration::from_secs(60);
    track(0).await;
    warnings
        .warn(&manager, start, Signal::Memory, &memory(&settings))
        .await;
    let delivered = manager
        .agent_messages(&manager.named("first").await)
        .await
        .unwrap();
    manager
        .mark_agent_messages_delivered(&manager.named("first").await, vec![delivered[0].id])
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
async fn only_disk_warnings_wake_an_idle_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    let agent = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &agent.plan.id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    let busy = SpaceSelector::id(&workspace.id);
    // A tracked command runs beside the agent.
    let _command = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let wakes = || async { manager.unread_messages(&workspace.id).await.unwrap().wakes };
    let settings = Overload::default();
    let mut warnings = Warnings::default();
    let now = Instant::now();
    warnings
        .warn(&manager, now, Signal::Memory, &memory(&settings))
        .await;
    warnings
        .warn(&manager, now, Signal::Cpu, &cpu(&settings))
        .await;
    assert_eq!(manager.agent_messages(&busy).await.unwrap().len(), 2);
    assert!(!wakes().await);
    // An agent can free disk space without running anything.
    warnings
        .warn(
            &manager,
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
        memory(&settings),
        cpu(&settings),
        disk(&settings, &format!("3.2 GiB available at {path}")),
    ] {
        crate::validate::message(&message).unwrap();
    }
}
