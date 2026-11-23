use crate::daemon::lifecycle::Removal;
use crate::daemon::space_service::tests::lifecycle::{fixture, removal_entries};
use crate::model::EventCause;
use crate::{
    daemon::{events::EventKind, workspace::reservation::RemovalRules},
    removal::{InspectionPolicy, NOTHING},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_removal_the_daemon_stopped_fails_its_workspace_for_shoal_rm() {
    let (_root, manager, workspace) = fixture().await;
    let selector = SpaceSelector::id(&workspace.id);
    // A removal reserved it, and the daemon stopped before it finished.
    manager
        .spaces
        .reserve_lifecycle_for_removal(&workspace.id, RemovalRules::default())
        .await
        .unwrap();
    (crate::daemon::events::record_removal_start(&manager.store, &workspace.id, EventCause::Idle))
        .await
        .unwrap();
    manager.spaces.interrupted_operations().await.unwrap();

    let failed = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(failed.state, WorkspaceState::Failed);
    let hint = format!("shoal rm {}", workspace.number);
    assert!(failed.part.error.unwrap().contains(&hint));
    assert_eq!(
        removal_entries(&manager, &selector).await,
        [
            (EventKind::RemovalStarted, Some(EventCause::Idle)),
            (EventKind::Retained, Some(EventCause::Idle)),
        ]
    );

    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            NOTHING.clone(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_removal_whose_work_ends_early_fails_its_workspace() {
    let (_root, manager, workspace) = fixture().await;
    let selector = SpaceSelector::id(&workspace.id);
    let removal = Removal::Manual {
        unsaved: &NOTHING,
        inspection: InspectionPolicy::GitOnly,
        release_holds: true,
        taking: None,
        for_repository: false,
        moved: None,
    };
    // Its work ends, as a panic ends it, before it removes or retains it.
    drop(
        manager
            .spaces
            .reserve_space(&workspace.id, removal)
            .await
            .unwrap(),
    );
    let failed = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let current = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
            if current.state != WorkspaceState::Removing {
                return current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(failed.state, WorkspaceState::Failed);
    let hint = format!("shoal rm {}", workspace.number);
    assert!(failed.part.error.unwrap().contains(&hint));
    // It ended before it began its work, which records the start.
    assert_eq!(
        removal_entries(&manager, &selector).await,
        [(EventKind::Retained, Some(EventCause::Manual))]
    );
}
