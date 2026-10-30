use super::*;
use crate::{
    daemon::scope::{self, Caller},
    model::HoldPlacer,
    protocol::HoldMethod,
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
            HoldPlacer::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .acquire_hold(
                &workspace.id,
                hold.name.clone(),
                Some("Changed reason".into()),
                HoldPlacer::User
            )
            .await
            .unwrap(),
        hold
    );
    manager
        .executions
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
    // Workspace processes list, ask for and release their own holds, but
    // never reach outside their tree.
    for target in [&workspace.name, &other.id] {
        let own = target == &workspace.name;
        let mut list = HoldMethod::HoldList {
            workspace: target.clone(),
        };
        let mut release = HoldMethod::HoldRelease {
            workspace: target.clone(),
            name: "app".into(),
        };
        let mut acquire = HoldMethod::HoldAcquire {
            workspace: target.clone(),
            name: "app".into(),
            reason: None,
        };
        for method in [&mut list, &mut release, &mut acquire] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), method)
                    .await
                    .is_ok(),
                own
            );
        }
    }
    // Releasing one the user placed is the user's.
    let error = manager
        .release_hold(&workspace.id, hold.name.clone(), HoldPlacer::Agent)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is the user's"), "{error:#}");
    assert!(
        manager
            .acquire_hold(&workspace.id, "bad name".into(), None, HoldPlacer::User)
            .await
            .is_err()
    );
    assert!(
        manager
            .acquire_hold(
                &workspace.id,
                "valid".into(),
                Some("bad\nreason".into()),
                HoldPlacer::User
            )
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
            .acquire_hold(&workspace.id, "too-late".into(), None, HoldPlacer::User)
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

#[tokio::test]
async fn release_remains_available_during_lifecycle_operations_and_hooks() {
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
    for name in ["first", "second"] {
        manager
            .acquire_hold(&workspace.id, name.into(), None, HoldPlacer::User)
            .await
            .unwrap();
    }
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    let _hook = manager
        .gates
        .resource_guard(&workspace.id, GuardMode::Exclusive)
        .unwrap();
    manager
        .release_hold(&workspace.id, "first".into(), HoldPlacer::User)
        .await
        .unwrap();
    manager
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    manager
        .release_hold(&workspace.id, "second".into(), HoldPlacer::User)
        .await
        .unwrap();
    manager
        .release_hold(&workspace.id, "second".into(), HoldPlacer::User)
        .await
        .unwrap();
    assert!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .holds
            .is_empty()
    );
}

#[tokio::test]
async fn agents_hold_the_workspaces_below_theirs_and_release_what_agents_placed() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    for (token, workspace) in [("parent", &parent), ("child", &child)] {
        manager
            .executions
            .issue_scope(
                token.into(),
                Caller {
                    workspace_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
    }
    let mut acquire = HoldMethod::HoldAcquire {
        workspace: child.name.clone(),
        name: "review".into(),
        reason: Some("Waiting for the parent's merge".into()),
    };
    scope::authorize(&manager, Some("parent"), &mut acquire)
        .await
        .unwrap();
    let hold = manager
        .acquire_hold(&child.id, "review".into(), None, HoldPlacer::Agent)
        .await
        .unwrap();
    assert_eq!(hold.placed_by, HoldPlacer::Agent);
    // An agent hold leaves the parent's agent free to remove the child.
    let mut remove = crate::protocol::RemovalMethod::RemoveWorkspace {
        workspace: child.name.clone(),
        unsaved: Default::default(),
    };
    scope::authorize(&manager, Some("parent"), &mut remove)
        .await
        .unwrap();
    // The child's own agent may release it, but not the user's.
    manager
        .acquire_hold(&child.id, "editor".into(), None, HoldPlacer::User)
        .await
        .unwrap();
    for name in ["review", "editor"] {
        let mut release = HoldMethod::HoldRelease {
            workspace: child.name.clone(),
            name: name.into(),
        };
        scope::authorize(&manager, Some("child"), &mut release)
            .await
            .unwrap();
    }
    manager
        .release_hold(&child.id, "review".into(), HoldPlacer::Agent)
        .await
        .unwrap();
    let error = manager
        .release_hold(&child.id, "editor".into(), HoldPlacer::Agent)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is the user's"), "{error:#}");
    let held = manager.workspace(&child.id).await.unwrap().holds;
    assert_eq!(
        held.iter()
            .map(|hold| hold.name.as_str())
            .collect::<Vec<_>>(),
        ["editor"]
    );
    // The user's hold refuses the parent's agent removing the child.
    let error = scope::authorize(&manager, Some("parent"), &mut remove)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("has holds"), "{error:#}");
}

#[tokio::test]
async fn an_agent_places_its_own_hold_only_while_approved() {
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
    let place = || {
        manager.place_hold(
            &workspace.id,
            "review".into(),
            None,
            HoldPlacer::Agent,
            Authority::Approval,
        )
    };
    let Ok(crate::daemon::allocation::Allocation::Approval(requested)) = manager
        .request_hold(&workspace.id, "review".into(), Some("Keep it".into()))
        .await
    else {
        panic!("holding its own workspace needs approval");
    };
    // Without an approval, as when a release lands between a request's
    // check and its hold, nothing is placed.
    let error = place().await.unwrap_err();
    assert!(
        error.to_string().contains("approval for hold review ended"),
        "{error:#}"
    );
    manager
        .decide_approval(requested.id, true, None, Default::default())
        .await
        .unwrap();
    place().await.unwrap();
    manager
        .release_hold(&workspace.id, "review".into(), HoldPlacer::Agent)
        .await
        .unwrap();
    assert!(place().await.is_err());
    assert!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .holds
            .is_empty()
    );
}
