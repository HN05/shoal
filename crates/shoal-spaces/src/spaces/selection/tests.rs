use crate::removal::{InspectionPolicy, UnsavedWork};
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_stored_id_never_selects_a_space_by_name() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .space(&manager.named("worker").await)
            .await
            .is_ok()
    );
    assert!(
        manager
            .spaces
            .space(&SpaceSelector::Id("worker".into()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_workspace_keeps_no_number() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let number = SpaceSelector::Number(worker.number);
    (manager.spaces.remove_selected(
        &*manager,
        &number,
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    assert!(manager.spaces.space(&number).await.is_err());
    assert!(manager.restore_workspace(&number).await.is_err());
    manager
        .restore_workspace(&manager.named("worker").await)
        .await
        .unwrap();
}
