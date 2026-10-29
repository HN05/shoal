use std::time::Duration;

use crate::{
    daemon::workspace::{ExecutionKind, Manager},
    execution::recovery,
    model::{AgentSession, Workspace},
    test_support::{manager, repository},
};

/// A tracked agent `name` in `workspace`, reporting `session` when given.
async fn agent(
    manager: &Manager,
    workspace: &Workspace,
    name: &str,
    session: Option<&str>,
) -> String {
    let id = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap()
        .plan
        .id;
    manager
        .track_agent(
            &id,
            name,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    if let Some(session) = session {
        recovery::save_session(&manager.paths, &workspace.id, &id, session).unwrap();
    }
    id
}

async fn refusal(manager: &Manager, workspace: &Workspace, caller: Option<&str>) -> String {
    let error = manager
        .agent_session(&workspace.name, caller)
        .await
        .unwrap_err();
    format!("{error:#}")
}

#[tokio::test]
async fn a_fork_starts_from_the_callers_agent_or_the_only_one() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let error = refusal(&manager, &workspace, None).await;
    assert!(error.contains("parent has no running agent"), "{error}");

    let quiet = agent(&manager, &workspace, "claude", None).await;
    let error = refusal(&manager, &workspace, None).await;
    assert!(
        error.contains("claude in parent has not reported"),
        "{error}"
    );

    recovery::save_session(&manager.paths, &workspace.id, &quiet, "s1").unwrap();
    let only = AgentSession {
        agent: "claude".into(),
        session: "s1".into(),
    };
    assert_eq!(
        manager.agent_session(&workspace.name, None).await.unwrap(),
        only
    );

    let own = agent(&manager, &workspace, "happy claude", Some("s2")).await;
    let error = refusal(&manager, &workspace, None).await;
    assert!(error.contains("parent runs several agents"), "{error}");
    // A caller outside the workspace's agents chooses none of them.
    let error = refusal(&manager, &workspace, Some("elsewhere")).await;
    assert!(error.contains("several agents"), "{error}");
    assert_eq!(
        manager
            .agent_session(&workspace.name, Some(&own))
            .await
            .unwrap(),
        AgentSession {
            agent: "happy-claude".into(),
            session: "s2".into(),
        }
    );
}
