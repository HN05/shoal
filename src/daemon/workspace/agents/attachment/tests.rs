use std::time::Duration;

use crate::{
    daemon::workspace::ExecutionKind,
    test_support::{manager, repository},
};

#[tokio::test]
async fn hooks_hear_each_change_of_attachment_once() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
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
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            "claude",
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    assert_eq!(manager.attachment_news(&id).await, None);
    // Another workspace cannot report for the agent.
    manager
        .report_attached(&other.name, &id, true)
        .await
        .unwrap();
    assert_eq!(manager.attachment_news(&id).await, None);
    manager
        .report_attached(&workspace.name, &id, true)
        .await
        .unwrap();
    assert_eq!(manager.attachment_news(&id).await, Some(true));
    manager.told_attached(Some(&id), Some(true)).await;
    assert_eq!(manager.attachment_news(&id).await, None);
    manager
        .report_attached(&workspace.name, &id, false)
        .await
        .unwrap();
    assert_eq!(manager.attachment_news(&id).await, Some(false));
}
