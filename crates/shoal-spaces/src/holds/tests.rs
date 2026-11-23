use crate::gates::GuardMode;
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;

use crate::{model::HoldPlacer, state::WorkspaceState};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn lifecycle_reservation_excludes_new_holds() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .acquire_hold(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                "too-late".into(),
                None,
                HoldPlacer::User
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .holds
            .is_empty()
    );
}

#[tokio::test]
async fn release_remains_available_during_lifecycle_operations_and_hooks() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    for name in ["first", "second"] {
        manager
            .spaces
            .acquire_hold(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                name.into(),
                None,
                HoldPlacer::User,
            )
            .await
            .unwrap();
    }
    manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    let _hook = manager
        .gates
        .resource_guard(&workspace.id, GuardMode::Exclusive)
        .unwrap();
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "first".into(),
            HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "second".into(),
            HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "second".into(),
            HoldPlacer::User,
        )
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .holds
            .is_empty()
    );
}
