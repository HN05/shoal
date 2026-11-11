use super::super::tests::{fixture, fixture_with_hook, fixture_with_removal_hook};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::notifications::NotificationKind,
    removal::{NOTHING, UnsavedWork},
    state::WorkspaceState,
};

fn everything() -> UnsavedWork {
    UnsavedWork::Confirmed {
        files: true,
        detached_head: None,
        files_restorable: false,
        children: None,
    }
}

#[tokio::test]
async fn returns_before_the_removal_finishes() {
    let (root, manager, workspace) = fixture_with_hook("hooks.pre_remove", |root| {
        format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done",
            root.join("go").display()
        )
    })
    .await;
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .start_workspace_removal(&selector, NOTHING.clone(), true, None)
        .await
        .unwrap();
    let reserved = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(reserved.state, WorkspaceState::Removing);
    assert!(workspace.path.exists());

    std::fs::write(root.path().join("go"), "").unwrap();
    drop(manager.gates.quiesce().await);
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn refuses_unconfirmed_work_before_returning() {
    let (_root, manager, workspace) = fixture().await;
    std::fs::write(workspace.path.join("draft.txt"), "unsaved").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let error = manager
        .start_workspace_removal(&selector, NOTHING.clone(), true, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("confirm it or pass --yes"));
    let retained = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(retained.state, WorkspaceState::Ready);

    manager
        .start_workspace_removal(&selector, everything(), true, None)
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_failure_retains_the_workspace_and_tells_the_user() {
    let (_root, manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .start_workspace_removal(&selector, NOTHING.clone(), true, None)
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);

    let retained = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(retained.state, WorkspaceState::Ready);
    assert!(workspace.path.exists());
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == NotificationKind::RemovalFailed)
    );
}

#[tokio::test]
async fn a_failure_reaches_the_requesting_agent() {
    let (_root, manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let requester = manager
        .create_workspace(
            &workspace.repository_id,
            "requester".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .start_workspace_removal(
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            false,
            Some(requester.id.clone()),
        )
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);

    let messages = manager
        .agent_messages(&SpaceSelector::id(&requester.id))
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.message.contains(workspace.name.as_str()))
    );
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != NotificationKind::RemovalFailed)
    );
}

#[tokio::test]
async fn a_failure_outlives_the_requesting_chat() {
    let (_root, manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let chat = manager
        .start_chat(
            workspace.repository_id.clone().into(),
            "requester".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    manager
        .start_workspace_removal(
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            false,
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);
    let failed = |notification: &crate::daemon::notifications::Notification| {
        notification.kind == NotificationKind::RemovalFailed
            && notification.workspace.as_ref() == Some(&workspace.name)
    };
    assert!(
        !manager
            .notifications(true, 10)
            .await
            .unwrap()
            .iter()
            .any(failed)
    );

    process.kill();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    assert!(
        manager
            .notifications(true, 10)
            .await
            .unwrap()
            .iter()
            .any(failed)
    );
}
