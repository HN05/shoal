use crate::config::overload::Overload;
use crate::daemon::warnings::*;
use crate::daemon::{
    executions::ExecutionKind,
    test_support::{manager, repository},
};
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
async fn running_agents_are_warned_once_per_signal_until_the_repeat_interval() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
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
                .agents
                .begin_execution(
                    &*manager,
                    workspace.id.as_str(),
                    None,
                    ExecutionKind::Command,
                    None,
                )
                .await
                .unwrap();
            // The execution's stop receiver keeps the agent connected.
            agents.push((workspace, execution));
        }
    }
    let claude = shoal_protocol::executions::TrackedAgent::named("claude");
    let track = |index: usize| {
        let (workspace, execution) = &agents[index];
        manager.agents.track_agent(
            &*manager,
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
                .agents
                .agent_messages(&*manager, &manager.named(name).await)
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
        .warn(
            &manager.agents,
            start,
            Signal::Memory,
            &memory(&settings, false),
        )
        .await;
    let delivered = manager
        .agents
        .agent_messages(&*manager, &manager.named("first").await)
        .await
        .unwrap();
    manager
        .agents
        .mark_agent_messages_delivered(
            &*manager,
            &manager.named("first").await,
            vec![delivered[0].id],
        )
        .await
        .unwrap();
    // Within the interval, only an agent started since is warned again,
    // and each signal has its own interval.
    track(1).await;
    warnings
        .warn(
            &manager.agents,
            later,
            Signal::Memory,
            &memory(&settings, false),
        )
        .await;
    warnings
        .warn(
            &manager.agents,
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
        .warn(
            &manager.agents,
            start + repeat,
            Signal::Memory,
            &memory(&settings, false),
        )
        .await;
    assert_eq!(count("first").await, 2);
}
