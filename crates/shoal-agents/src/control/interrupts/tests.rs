use crate::executions::ExecutionKind;
use crate::model::Workspace;
use crate::protocol::TurnEnd;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;

/// A tracked agent `name` in `workspace`.
async fn agent(fixture: &Arc<Fixture>, workspace: &Workspace, name: &str) -> ExecutionId {
    let id = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap()
        .plan
        .id;
    fixture
        .agents
        .track_agent(
            &*fixture.daemon,
            &id,
            &shoal_protocol::executions::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    id
}

async fn workspace(fixture: &Arc<Fixture>, name: &str) -> Workspace {
    fixture.workspace(name).await
}

/// The turn state and interruption of the workspace's only agent.
async fn turn(fixture: &Arc<Fixture>, workspace: &Workspace) -> (Option<AgentState>, bool) {
    let agents = fixture
        .agents
        .space_agents(&*fixture.daemon, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected one agent: {agents:?}");
    };
    (agent.state, agent.interrupted)
}

#[tokio::test]
async fn an_interrupt_leaves_the_agent_idle_until_a_turn_starts_or_ends() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture, "fix").await;
    let id = agent(&fixture, &workspace, "claude").await;
    let selected = SpaceSelector::id(&workspace.id);
    let report = |state| {
        fixture
            .agents
            .set_agent_state(&*fixture.daemon, &selected, state, Some(id.clone()), None)
    };
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&fixture, &workspace).await,
        (Some(AgentState::Working), false)
    );

    let status = fixture
        .agents
        .interrupt_turn(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
    assert_eq!(
        turn(&fixture, &workspace).await,
        (Some(AgentState::Idle), true)
    );
    assert!(fixture.agents.turn_interrupted(&id).await);

    // Claude Code's notice of a waiting prompt reports idle again.
    report(AgentState::Idle).await.unwrap();
    assert!(fixture.agents.turn_interrupted(&id).await);
    report(AgentState::Working).await.unwrap();
    assert_eq!(
        turn(&fixture, &workspace).await,
        (Some(AgentState::Working), false)
    );

    fixture
        .agents
        .interrupt_turn(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            id.clone(),
        )
        .await
        .unwrap();
    fixture
        .agents
        .end_agent_turn(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            Some(id.clone()),
            None,
            TurnEnd::default(),
        )
        .await
        .unwrap();
    assert!(!fixture.agents.turn_interrupted(&id).await);
}

#[tokio::test]
async fn an_interrupt_names_an_agent_of_the_workspace() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture, "fix").await;
    let other = workspace_with_agent(&fixture).await;
    fixture
        .agents
        .interrupt_turn(&*fixture.daemon, &SpaceSelector::id(&workspace.id), other)
        .await
        .unwrap_err();
}

async fn workspace_with_agent(fixture: &Arc<Fixture>) -> ExecutionId {
    let other = workspace(fixture, "other").await;
    agent(fixture, &other, "claude").await
}
