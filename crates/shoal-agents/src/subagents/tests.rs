use super::*;
use crate::{model::Workspace, stand_in::Fixture, state::AgentState};

/// A workspace whose tracked agent is idle, and the execution it runs in.
async fn idle_agent(fixture: &Fixture) -> (Workspace, ExecutionId) {
    let workspace = fixture.workspace("idle").await;
    let id = fixture.execution(&workspace, "agent", None).await;
    let space = SpaceSelector::id(&workspace.id);
    (fixture.agents)
        .set_agent_state(
            &fixture.daemon,
            &space,
            AgentState::Idle,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    (workspace, id)
}

/// Report `change` to the subagents of `execution` in `workspace` at `at`.
async fn report(
    fixture: &Fixture,
    workspace: &Workspace,
    execution: &ExecutionId,
    change: SubagentChange,
    at: UnixTime,
) {
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .report_subagent(&fixture.daemon, &space, Some(execution.clone()), change, at))
    .await
    .unwrap();
}

/// The turn state protection reads for `execution`.
async fn state(fixture: &Fixture, execution: &ExecutionId) -> AgentState {
    fixture.agents.agent_turns(&fixture.daemon).await.unwrap()[execution].state
}

fn started(agent_id: &str) -> SubagentChange {
    SubagentChange::Started {
        agent_id: agent_id.into(),
    }
}

fn stopped(agent_id: &str) -> SubagentChange {
    SubagentChange::Stopped {
        agent_id: agent_id.into(),
    }
}

#[tokio::test]
async fn an_idle_agent_works_while_one_of_its_subagents_runs() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let now = UnixTime::now();
    for agent_id in ["a1", "a2"] {
        report(&fixture, &workspace, &id, started(agent_id), now).await;
    }
    assert_eq!(state(&fixture, &id).await, AgentState::Working);

    report(&fixture, &workspace, &id, stopped("a1"), now).await;
    assert_eq!(state(&fixture, &id).await, AgentState::Working);
    report(&fixture, &workspace, &id, stopped("a2"), now).await;
    assert_eq!(state(&fixture, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_subagent_whose_stop_is_lost_counts_until_its_requests_stop() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let long_ago = UnixTime::now() - LOST_AFTER - Duration::from_secs(60);
    report(&fixture, &workspace, &id, started("a1"), long_ago).await;
    assert_eq!(
        state(&fixture, &id).await,
        AgentState::Working,
        "without a proxy, it counts until its stop"
    );

    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .report_subagent_request(&fixture.daemon, &space, id.clone(), long_ago))
    .await
    .unwrap();
    assert_eq!(state(&fixture, &id).await, AgentState::Idle);

    (fixture
        .agents
        .report_subagent_request(&fixture.daemon, &space, id.clone(), UnixTime::now()))
    .await
    .unwrap();
    assert_eq!(state(&fixture, &id).await, AgentState::Working);
}

#[tokio::test]
async fn a_session_start_ends_the_subagents_of_the_session_before() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let now = UnixTime::now();
    report(&fixture, &workspace, &id, started("a1"), now).await;
    report(
        &fixture,
        &workspace,
        &id,
        SubagentChange::SessionStarted,
        now,
    )
    .await;
    assert_eq!(state(&fixture, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_stop_that_arrives_before_its_start_leaves_the_subagent_stopped() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let now = UnixTime::now();
    report(&fixture, &workspace, &id, stopped("a1"), now).await;
    report(&fixture, &workspace, &id, started("a1"), now).await;
    assert_eq!(state(&fixture, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_resumed_subagent_runs_again_until_its_next_stop() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let now = UnixTime::now();
    for change in [started("a1"), stopped("a1"), started("a1")] {
        report(&fixture, &workspace, &id, change, now).await;
    }
    assert_eq!(state(&fixture, &id).await, AgentState::Working);
    report(&fixture, &workspace, &id, stopped("a1"), now).await;
    assert_eq!(state(&fixture, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_session_start_forgets_the_requests_its_proxy_passed_before() {
    let fixture = Fixture::new().await;
    let (workspace, id) = idle_agent(&fixture).await;
    let long_ago = UnixTime::now() - LOST_AFTER - Duration::from_secs(60);
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .report_subagent_request(&fixture.daemon, &space, id.clone(), long_ago))
    .await
    .unwrap();
    // A restore in place started the session without the proxy.
    report(
        &fixture,
        &workspace,
        &id,
        SubagentChange::SessionStarted,
        long_ago,
    )
    .await;
    report(&fixture, &workspace, &id, started("a1"), long_ago).await;
    assert_eq!(state(&fixture, &id).await, AgentState::Working);
}
