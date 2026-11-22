use crate::execution::recovery;
use crate::executions::ExecutionKind;
use crate::model::AgentSession;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;

/// A tracked agent `name` in `workspace`, reporting `session` when given.
async fn agent(
    fixture: &Arc<Fixture>,
    workspace: &Workspace,
    name: &str,
    session: Option<&str>,
) -> ExecutionId {
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
    if let Some(session) = session {
        recovery::save_session(&fixture.agents.paths, &workspace.id, &id, &session.into()).unwrap();
    }
    id
}

async fn refused(
    fixture: &Arc<Fixture>,
    workspace: &Workspace,
    caller: Option<&ExecutionId>,
) -> bool {
    fixture
        .agents
        .agent_session(&*fixture.daemon, &SpaceSelector::id(&workspace.id), caller)
        .await
        .is_err()
}

#[tokio::test]
async fn a_fork_starts_from_the_callers_agent_or_the_only_one() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("parent").await;
    assert!(refused(&fixture, &workspace, None).await);

    let quiet = agent(&fixture, &workspace, "claude", None).await;
    assert!(refused(&fixture, &workspace, None).await);

    recovery::save_session(&fixture.agents.paths, &workspace.id, &quiet, &"s1".into()).unwrap();
    let only = AgentSession {
        agent: "claude".into(),
        session: "s1".into(),
    };
    assert_eq!(
        fixture
            .agents
            .agent_session(&*fixture.daemon, &SpaceSelector::id(&workspace.id), None)
            .await
            .unwrap(),
        only
    );

    let own = agent(&fixture, &workspace, "happy claude", Some("s2")).await;
    assert!(refused(&fixture, &workspace, None).await);
    // A caller outside the workspace's agents chooses none of them.
    assert!(refused(&fixture, &workspace, Some(&ExecutionId::from("elsewhere"))).await);
    assert_eq!(
        fixture
            .agents
            .agent_session(
                &*fixture.daemon,
                &SpaceSelector::id(&workspace.id),
                Some(&own)
            )
            .await
            .unwrap(),
        AgentSession {
            agent: "happy-claude".into(),
            session: "s2".into(),
        }
    );
}
