use crate::daemon::scope::Access;
use crate::daemon::space_service::tests::lifecycle::fixture_with_removal_hook;
use crate::removal::InspectionPolicy;
use crate::{
    daemon::{notifications::AlertKind, scope::Caller},
    removal::{NOTHING, UnsavedWork},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

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
