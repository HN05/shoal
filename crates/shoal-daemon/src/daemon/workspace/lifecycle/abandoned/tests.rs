use super::super::{
    Removal,
    tests::{fixture, removal_entries},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{EventCause, EventKind},
        workspace::reservation::RemovalRules,
    },
    removal::{InspectionPolicy, NOTHING},
    state::WorkspaceState,
};

#[tokio::test]
async fn a_removal_the_daemon_stopped_fails_its_workspace_for_shoal_rm() {
    let (_root, manager, workspace) = fixture().await;
    let selector = SpaceSelector::id(&workspace.id);
    // A removal reserved it, and the daemon stopped before it finished.
    manager
        .reserve_lifecycle_for_removal(&workspace.id, RemovalRules::default())
        .await
        .unwrap();
    (manager.record_removal_start(&workspace.id, EventCause::Idle))
        .await
        .unwrap();
    manager
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();

    let failed = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(failed.state, WorkspaceState::Failed);
    let hint = format!("shoal rm {}", workspace.number);
    assert!(failed.error.unwrap().contains(&hint));
    assert_eq!(
        removal_entries(&manager, &selector).await,
        [
            (EventKind::RemovalStarted, Some(EventCause::Idle)),
            (EventKind::Retained, Some(EventCause::Idle)),
        ]
    );

    manager
        .remove_workspace(&selector, NOTHING.clone(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
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
    };
    // Its work ends, as a panic ends it, before it removes or retains it.
    drop(manager.reserve_space(&workspace.id, removal).await.unwrap());
    let failed = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let current = manager.workspace_by_id(&workspace.id).await.unwrap();
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
    assert!(failed.error.unwrap().contains(&hint));
    // It ended before it began its work, which records the start.
    assert_eq!(
        removal_entries(&manager, &selector).await,
        [(EventKind::Retained, Some(EventCause::Manual))]
    );
}
