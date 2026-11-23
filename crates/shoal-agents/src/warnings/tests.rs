use crate::config::overload::Overload;
use crate::executions::ExecutionKind;
use crate::model::SpaceKind;
use crate::stand_in::Fixture;
use crate::warnings::*;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
async fn only_disk_warnings_wake_an_idle_agent_and_only_in_a_workspace() {
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
    let chat = fixture.chat("talk").await;
    let _chat_agent = fixture
        .agents
        .track_unconnected(
            &chat.as_str().into(),
            &chat,
            &"talk".into(),
            crate::agent::Agent::from("claude"),
        )
        .await;
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
    let unread = |space| {
        let agents = &fixture.agents;
        async move { agents.unread_messages(space).await.unwrap() }
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
    assert!(!unread(&workspace.id).await.wakes);
    assert!(!unread(&chat).await.wakes);
    // An agent can free disk space without running anything.
    warnings
        .warn(
            &fixture.agents,
            now,
            Signal::Disk,
            &disk(&settings, "1.0 GiB available at /"),
        )
        .await;
    let workspace_unread = unread(&workspace.id).await;
    assert!(workspace_unread.wakes);
    assert!(!workspace_unread.alerts[2].message.ends_with(HEADS_UP));
    // A chat has no worktree to free space in: its agent reads the warning
    // as a heads-up at its next turn.
    let chat_unread = unread(&chat).await;
    assert!(!chat_unread.wakes);
    assert!(chat_unread.alerts[2].message.ends_with(HEADS_UP));
}

#[test]
fn warnings_are_single_lines_within_the_message_limit() {
    let settings = Overload::default();
    let path = format!("/{}", "x".repeat(200));
    for warning in [
        memory(&settings, false),
        memory(&settings, true),
        cpu(&settings),
        disk(&settings, &format!("3.2 GiB available at {path}")),
    ] {
        for kind in [SpaceKind::Workspace, SpaceKind::Chat] {
            crate::validate::message(&warning.for_space(kind).0).unwrap();
        }
    }
}
