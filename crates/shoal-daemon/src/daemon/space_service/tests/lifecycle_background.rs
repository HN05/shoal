use crate::daemon::scope::Access;
use crate::daemon::space_service::tests::lifecycle::{
    fixture, fixture_with_hook, fixture_with_removal_hook,
};
use crate::removal::InspectionPolicy;
use crate::{
    daemon::{notifications::AlertKind, scope::Caller},
    removal::{NOTHING, UnsavedWork},
    state::WorkspaceState,
};
use shoal_ids::space_ids::SpaceId;
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
    let (root, manager, workspace) = fixture_with_hook("hooks.pre_remove", |root| {
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

    std::fs::write(root.path().join("go"), "").unwrap();
    drop(manager.gates.quiesce().await);
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn refuses_unconfirmed_work_before_returning() {
    let (_root, manager, workspace) = fixture().await;
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
    let (_root, manager, workspace) = fixture_with_removal_hook("exit 1").await;
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
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::RemovalFailed)
    );
}

#[tokio::test]
async fn a_failure_reaches_the_requesting_agent() {
    let (_root, manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let requester = manager
        .create_workspace(workspace.repository(), "requester".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .start_removal(
            &manager.daemon(),
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            Some(&caller_in(&requester.id)),
            false,
        )
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);

    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&requester.id))
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.text().contains(workspace.name.as_str()))
    );
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != AlertKind::RemovalFailed)
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
            workspace.repository().clone().into(),
            "requester".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .start_removal(
            &manager.daemon(),
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            Some(&caller_in(&chat.id)),
            false,
        )
        .await
        .unwrap();
    drop(manager.gates.quiesce().await);
    let failed = |notification: &crate::daemon::notifications::Alert| {
        notification.kind == AlertKind::RemovalFailed
            && notification.space.as_ref() == Some(&workspace.name)
    };
    assert!(
        !manager
            .notifications
            .list(true, 10)
            .await
            .unwrap()
            .iter()
            .any(failed)
    );

    process.kill();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(
        manager
            .notifications
            .list(true, 10)
            .await
            .unwrap()
            .iter()
            .any(failed)
    );
}

/// A scoped caller in the space `id`, without an execution.
fn caller_in(id: &SpaceId) -> Caller {
    Caller {
        space_id: id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    }
}
