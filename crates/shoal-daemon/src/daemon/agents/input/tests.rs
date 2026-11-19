use std::time::Duration;

use super::*;
use crate::daemon::{
    executions::ExecutionKind,
    test_support::{manager, repository},
    workspace::Manager,
};
use crate::model::Workspace;

/// Two workspaces, the first with a tracked agent, and the execution that
/// agent runs in.
async fn agent(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace, ExecutionId) {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    (workspace, other, id)
}

async fn interacted_at(manager: &Manager, workspace: &Workspace) -> Option<UnixTime> {
    let interactions = manager.interactions().await.unwrap();
    interactions.get(&workspace.id).copied()
}

#[tokio::test]
async fn only_the_agents_space_reports_the_keys_it_reads() {
    let (root, manager) = manager().await;
    let (workspace, other, id) = agent(&manager, root.path()).await;
    let at = UnixTime::from_seconds(1000);
    for space in [&other, &workspace] {
        (manager.report_input(&SpaceSelector::id(&space.id), &id, at))
            .await
            .unwrap();
    }
    assert_eq!(interacted_at(&manager, &other).await, None);
    assert_eq!(interacted_at(&manager, &workspace).await, Some(at));
}

#[tokio::test]
async fn only_the_first_key_read_after_shoal_types_is_shoals() {
    let (root, manager) = manager().await;
    let (workspace, _, id) = agent(&manager, root.path()).await;
    let space = SpaceSelector::id(&workspace.id);
    manager.report_typing(&space, &id).await.unwrap();
    let typed = (manager.executions)
        .with_agent(&id, |agent| agent.typed_at)
        .await
        .flatten()
        .unwrap();
    manager.report_input(&space, &id, typed).await.unwrap();
    assert_eq!(interacted_at(&manager, &workspace).await, None);

    // The user types right after it.
    let user = typed + Duration::from_secs(1);
    manager.report_input(&space, &id, user).await.unwrap();
    assert_eq!(interacted_at(&manager, &workspace).await, Some(user));
}

#[tokio::test]
async fn a_key_after_shoals_typing_moved_nothing_is_the_users() {
    let (root, manager) = manager().await;
    let (workspace, _, id) = agent(&manager, root.path()).await;
    let space = SpaceSelector::id(&workspace.id);
    manager.report_typing(&space, &id).await.unwrap();
    let typed = (manager.executions)
        .with_agent(&id, |agent| agent.typed_at)
        .await
        .flatten()
        .unwrap();
    // Shoal's keys left the access time where it was, and the user's key
    // eight seconds later moved it.
    let user = typed + Duration::from_secs(8);
    manager.report_input(&space, &id, user).await.unwrap();
    assert_eq!(interacted_at(&manager, &workspace).await, Some(user));
}
