use crate::executions::ExecutionKind;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use crate::time::UnixTime;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;

/// Two workspaces, the first with a tracked agent, and the execution that
/// agent runs in.
async fn agent(fixture: &Arc<Fixture>) -> (Workspace, Workspace, ExecutionId) {
    let workspace = fixture.workspace("agent").await;
    let other = fixture.workspace("other").await;
    let execution = fixture
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
    let id = execution.plan.id.clone();
    fixture
        .agents
        .track_agent(
            &*fixture.daemon,
            &id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    (workspace, other, id)
}

async fn interacted_at(fixture: &Arc<Fixture>, workspace: &Workspace) -> Option<UnixTime> {
    let interactions = fixture.agents.interactions().await.unwrap();
    interactions.get(&workspace.id).copied()
}

#[tokio::test]
async fn only_the_agents_space_reports_the_keys_it_reads() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, other, id) = agent(&fixture).await;
    let at = UnixTime::from_seconds(1000);
    for space in [&other, &workspace] {
        (fixture
            .agents
            .report_input(&*fixture.daemon, &SpaceSelector::id(&space.id), &id, at))
        .await
        .unwrap();
    }
    assert_eq!(interacted_at(&fixture, &other).await, None);
    assert_eq!(interacted_at(&fixture, &workspace).await, Some(at));
}

#[tokio::test]
async fn only_the_first_key_read_after_shoal_types_is_shoals() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, _, id) = agent(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    fixture
        .agents
        .report_typing(&*fixture.daemon, &space, &id)
        .await
        .unwrap();
    let typed = (fixture.agents.executions())
        .with_agent(&id, |agent| agent.typed_at)
        .await
        .flatten()
        .unwrap();
    fixture
        .agents
        .report_input(&*fixture.daemon, &space, &id, typed)
        .await
        .unwrap();
    assert_eq!(interacted_at(&fixture, &workspace).await, None);

    // The user types right after it.
    let user = typed + Duration::from_secs(1);
    fixture
        .agents
        .report_input(&*fixture.daemon, &space, &id, user)
        .await
        .unwrap();
    assert_eq!(interacted_at(&fixture, &workspace).await, Some(user));
}

#[tokio::test]
async fn a_key_after_shoals_typing_moved_nothing_is_the_users() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, _, id) = agent(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    fixture
        .agents
        .report_typing(&*fixture.daemon, &space, &id)
        .await
        .unwrap();
    let typed = (fixture.agents.executions())
        .with_agent(&id, |agent| agent.typed_at)
        .await
        .flatten()
        .unwrap();
    // Shoal's keys left the access time where it was, and the user's key
    // eight seconds later moved it.
    let user = typed + Duration::from_secs(8);
    fixture
        .agents
        .report_input(&*fixture.daemon, &space, &id, user)
        .await
        .unwrap();
    assert_eq!(interacted_at(&fixture, &workspace).await, Some(user));
}
