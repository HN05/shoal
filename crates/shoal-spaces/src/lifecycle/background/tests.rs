use crate::lifecycle::tests::{fixture, fixture_with_hook, fixture_with_removal_hook};
use crate::{
    notifications::AlertKind,
    removal::{NOTHING, UnsavedWork},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

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
    let (manager, workspace) = fixture_with_hook("hooks.pre_remove", |root| {
        format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done",
            root.join("go").display()
        )
    })
    .await;
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .spaces
        .start_removal(&manager.daemon(), &selector, NOTHING.clone(), None, true)
        .await
        .unwrap();
    let reserved = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(reserved.state, WorkspaceState::Removing);
    assert!(workspace.path.exists());

    std::fs::write(manager.root.path().join("go"), "").unwrap();
    drop(manager.gates.quiesce().await);
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn refuses_unconfirmed_work_before_returning() {
    let (manager, workspace) = fixture().await;
    std::fs::write(workspace.path.join("draft.txt"), "unsaved").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let error = manager
        .spaces
        .start_removal(&manager.daemon(), &selector, NOTHING.clone(), None, true)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("confirm it or pass --yes"));
    let retained = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(retained.state, WorkspaceState::Ready);

    manager
        .spaces
        .start_removal(&manager.daemon(), &selector, everything(), None, true)
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_failure_retains_the_workspace_and_tells_the_user() {
    let (manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .spaces
        .start_removal(&manager.daemon(), &selector, NOTHING.clone(), None, true)
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);

    let retained = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(retained.state, WorkspaceState::Ready);
    assert!(workspace.path.exists());
    let notifications = manager.spaces.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::RemovalFailed)
    );
}
