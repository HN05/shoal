use super::*;
use crate::{
    daemon::scope::{self, Caller},
    protocol::Method,
    state::WorkspaceState,
    test_support::{manager, repository},
};

#[tokio::test]
async fn holds_are_idempotent_persistent_and_scoped() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "held".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let hold = manager
        .acquire_hold(
            &workspace.id,
            "app-thread".into(),
            Some("Open thread".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .acquire_hold(
                &workspace.id,
                hold.name.clone(),
                Some("Changed reason".into())
            )
            .await
            .unwrap(),
        hold
    );
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: workspace.id.clone(),
                execution: Some(crate::daemon::scope::ScopedExecution {
                    id: "execution".into(),
                    kind: super::super::ExecutionKind::Command,
                }),
            },
        )
        .await;
    for target in [&workspace.name, &other.id] {
        for mut method in [
            Method::HoldAcquire {
                workspace: target.clone(),
                name: "app".into(),
                reason: None,
            },
            Method::HoldRelease {
                workspace: target.clone(),
                name: "app".into(),
            },
            Method::HoldList {
                workspace: target.clone(),
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), &mut method)
                    .await
                    .is_ok(),
                target == &workspace.name
            );
        }
    }
    assert!(
        manager
            .acquire_hold(&workspace.id, "bad name".into(), None)
            .await
            .is_err()
    );
    assert!(
        manager
            .acquire_hold(&workspace.id, "valid".into(), Some("bad\nreason".into()))
            .await
            .is_err()
    );
    let paths = manager.paths.clone();
    manager.store.shutdown().await;
    drop(manager);
    let reopened = Manager::open(paths).await.unwrap();
    assert_eq!(
        reopened.workspace(&workspace.id).await.unwrap().holds,
        vec![hold]
    );
    assert_eq!(
        reopened
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .find(|w| w.id == workspace.id)
            .unwrap()
            .holds
            .len(),
        1
    );
}

#[tokio::test]
async fn lifecycle_reservation_excludes_new_holds() {
    let (temp, manager) = manager().await;
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
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    assert!(
        manager
            .acquire_hold(&workspace.id, "too-late".into(), None)
            .await
            .is_err()
    );
    assert!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .holds
            .is_empty()
    );
}
