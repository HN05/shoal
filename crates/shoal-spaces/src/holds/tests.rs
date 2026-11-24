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

/// A chat of a new repository `repo`.
async fn chat(manager: &Fixture) -> crate::model::Chat {
    let repo = manager.repository("repo").await;
    manager.chat("audit", "chat", Some(&repo), None).await
}

#[tokio::test]
async fn a_hold_keeps_a_chat_until_it_is_released() {
    let manager = Fixture::new().await;
    let chat = chat(&manager).await;
    let selected = SpaceSelector::id(&chat.id);
    let hold =
        (manager
            .spaces
            .acquire_hold(&*manager, &selected, "notes".into(), None, HoldPlacer::User))
        .await
        .unwrap();
    assert_eq!(hold.space_id, chat.id);
    assert_eq!(manager.spaces.holds(&selected).await.unwrap(), [hold]);
    // Only the hold keeps it, as its release shows below.
    assert!(
        manager
            .spaces
            .keep_reason(&*manager, &chat.id)
            .await
            .unwrap()
            .is_some()
    );
    // A removal that leaves the user's holds in place refuses it.
    let parent = crate::lifecycle::Removal::Parent {
        cause: crate::model::EventCause::Parent,
        refuse_holds: true,
        confirmed: None,
        moved: None,
        lives_on: false,
    };
    let Err(refused) = manager.spaces.reserve_space(&chat.id, parent).await else {
        panic!("the user's hold keeps the chat");
    };
    assert!(format!("{refused:#}").contains("holds"), "{refused:#}");
    manager
        .spaces
        .release_hold(&selected, "notes".into(), HoldPlacer::User)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .keep_reason(&*manager, &chat.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_removal_reserving_a_chat_takes_no_new_hold() {
    let manager = Fixture::new().await;
    let chat = chat(&manager).await;
    let _removal = manager.spaces.space_removals.reserve(&chat.id).unwrap();
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&chat.id),
            "late".into(),
            None,
            HoldPlacer::User,
        )
        .await
        .unwrap_err();
    assert!(!manager.spaces.has_holds(&chat.id).await.unwrap());
}
