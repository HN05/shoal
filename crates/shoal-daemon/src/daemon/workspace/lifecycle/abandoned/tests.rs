use super::super::tests::{fixture, removal_entries};
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
