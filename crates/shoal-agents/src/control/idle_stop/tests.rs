use std::time::Duration;

use super::*;
use crate::executions::StartedExecution;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_protocol::space_names::SpaceSelector;

/// A time once the wait of an agent the user is not engaged with has
/// passed, whose wait is seconds.
fn later() -> UnixTime {
    UnixTime::now() + Duration::from_secs(60)
}

/// An idle chat whose agent runs in `mode`, in the fixture's repository
/// with a heavy chat mode `pair`, and its agent's execution.
async fn idle_chat(fixture: &Fixture, mode: &str) -> StartedExecution {
    let repo = fixture.repository().await;
    fixture.configure_repository(&repo.id, "[modes.pair]\nkind = 'chat'\n");
    let (chat, execution) = fixture.chat_agent("talk", mode, &repo).await;
    let selector = SpaceSelector::id(&chat);
    let id = Some(execution.plan.id.clone());
    (fixture.agents)
        .set_agent_state(&*fixture.daemon, &selector, AgentState::Idle, id, None)
        .await
        .unwrap();
    execution
}

/// A heavy chat's idle agent sleeps as a workspace's does.
#[tokio::test]
async fn a_heavy_chats_idle_agent_sleeps() {
    let fixture = Fixture::new().await;
    let execution = idle_chat(&fixture, "pair").await;

    (fixture.agents.stop_idle_agents(&*fixture.daemon, later()))
        .await
        .unwrap();

    assert!(*execution.stop.borrow());
}

/// A light chat's idle agent keeps running.
#[tokio::test]
async fn a_light_chats_idle_agent_keeps_running() {
    let fixture = Fixture::new().await;
    let execution = idle_chat(&fixture, "chat").await;

    (fixture.agents.stop_idle_agents(&*fixture.daemon, later()))
        .await
        .unwrap();

    assert!(!*execution.stop.borrow());
}
