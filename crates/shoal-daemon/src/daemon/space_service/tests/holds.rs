use crate::daemon::holds::*;
use crate::daemon::manager::Manager;
use crate::daemon::scope::Access;

use crate::{
    daemon::{
        scope::{self, Caller},
        test_support::{manager, repository},
    },
    model::HoldPlacer,
    protocol::HoldMethod,
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn holds_are_idempotent_persistent_and_scoped() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "app-thread".into(),
            Some("Open thread".into()),
            HoldPlacer::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .acquire_hold(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                hold.name.clone(),
                Some("Changed reason".into()),
                HoldPlacer::User
            )
            .await
            .unwrap(),
        hold
    );
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: Some(shoal_coordination::scope::ScopedExecution {
                    id: "execution".into(),
                    kind: crate::daemon::executions::ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    // Workspace processes list, ask for and release their own holds, but
    // never reach outside their tree.
    for target in [workspace.id.as_str(), other.id.as_str()] {
        let own = workspace.id == target;
        let mut list = HoldMethod::HoldList {
            space: SpaceSelector::from(target),
        };
        let mut release = HoldMethod::HoldRelease {
            space: SpaceSelector::from(target),
            name: "app".into(),
        };
        let mut acquire = HoldMethod::HoldAcquire {
            space: SpaceSelector::from(target),
            name: "app".into(),
            reason: None,
        };
        for method in [&mut list, &mut release, &mut acquire] {
            assert_eq!(
                scope::authorize(&manager, Some(&"scope".into()), method)
                    .await
                    .is_ok(),
                own
            );
        }
    }
    // Releasing one the user placed is the user's.
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            hold.name.clone(),
            HoldPlacer::Agent,
        )
        .await
        .unwrap_err();
    assert!(
        manager
            .spaces
            .acquire_hold(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                "bad name".into(),
                None,
                HoldPlacer::User
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .spaces
            .acquire_hold(
                &*manager,
                &SpaceSelector::id(&workspace.id),
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
        reopened
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .holds,
        vec![hold]
    );
    assert_eq!(
        reopened
            .spaces
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
async fn agents_hold_the_workspaces_below_theirs_and_release_what_agents_placed() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for (token, workspace) in [("parent", &parent), ("child", &child)] {
        manager
            .agents
            .executions()
            .issue_scope(
                token.into(),
                Caller {
                    space_id: workspace.id.clone(),
                    execution: None,
                    granted: None,
                    access: Access::Full,
                    vouched: None,
                },
            )
            .await;
    }
    let mut acquire = HoldMethod::HoldAcquire {
        space: SpaceSelector::id(&child.id),
        name: "review".into(),
        reason: Some("Waiting for the parent's merge".into()),
    };
    scope::authorize(&manager, Some(&"parent".into()), &mut acquire)
        .await
        .unwrap();
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&child.id),
            "review".into(),
            None,
            HoldPlacer::Agent,
        )
        .await
        .unwrap();
    // An agent hold leaves the parent's agent free to remove the child.
    let mut remove = crate::protocol::RemovalMethod::RemoveSpace {
        space: SpaceSelector::id(&child.id),
        unsaved: Default::default(),
        keep_user_holds: false,
        above: None,
    };
    scope::authorize(&manager, Some(&"parent".into()), &mut remove)
        .await
        .unwrap();
    // The child's own agent may release it, but not the user's.
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&child.id),
            "editor".into(),
            None,
            HoldPlacer::User,
        )
        .await
        .unwrap();
    for name in ["review", "editor"] {
        let mut release = HoldMethod::HoldRelease {
            space: SpaceSelector::id(&child.id),
            name: name.into(),
        };
        scope::authorize(&manager, Some(&"child".into()), &mut release)
            .await
            .unwrap();
    }
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&child.id),
            "review".into(),
            HoldPlacer::Agent,
        )
        .await
        .unwrap();
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&child.id),
            "editor".into(),
            HoldPlacer::Agent,
        )
        .await
        .unwrap_err();
    let held = manager
        .spaces
        .workspace_by_id(&child.id)
        .await
        .unwrap()
        .holds;
    assert_eq!(
        held.iter()
            .map(|hold| hold.name.as_str())
            .collect::<Vec<_>>(),
        ["editor"]
    );
    // The user's hold refuses the parent's agent removing the child, from
    // the check that comes before the removal.
    let mut check = crate::protocol::RemovalMethod::CheckRemoval {
        space: SpaceSelector::id(&child.id),
        include_changes: false,
    };
    scope::authorize(&manager, Some(&"parent".into()), &mut check)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn an_agent_places_its_own_hold_only_while_approved() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    let place = || {
        manager.spaces.place_hold(
            &*manager,
            &selected,
            "review".into(),
            None,
            HoldPlacer::Agent,
            Authority::Approval,
        )
    };
    let Ok(crate::daemon::allocation::Allocation::Approval(requested)) = manager
        .spaces
        .request_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            Some("Keep it".into()),
        )
        .await
    else {
        panic!("holding its own workspace needs approval");
    };
    // Without an approval, as when a release lands between a request's
    // check and its hold, nothing is placed.
    place().await.unwrap_err();
    manager
        .coordination
        .decide_approval(
            &*manager,
            requested.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    place().await.unwrap();
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            HoldPlacer::Agent,
        )
        .await
        .unwrap();
    assert!(place().await.is_err());
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

/// A chat in a fresh repository, with what keeps it: its holds alone.
async fn chat(manager: &std::sync::Arc<Manager>, root: &std::path::Path) -> crate::model::Chat {
    let checkout = repository(root, "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .start_chat(repo.id.into(), "audit".into(), "claude".into(), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_chats_agent_holds_its_own_chat_once_approved() {
    let (temp, manager) = manager().await;
    let chat = chat(&manager, temp.path()).await;
    let selected = SpaceSelector::id(&chat.id);
    let Ok(crate::daemon::allocation::Allocation::Approval(requested)) =
        (manager.spaces.request_hold(
            &*manager,
            &selected,
            "review".into(),
            Some("Keep it".into()),
        ))
        .await
    else {
        panic!("holding its own chat needs approval");
    };
    assert_eq!(requested.space_id, chat.id);
    assert!(!manager.spaces.has_holds(&chat.id).await.unwrap());
    manager
        .coordination
        .decide_approval(
            &*manager,
            requested.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    let holds = manager.spaces.holds(&selected).await.unwrap();
    assert_eq!(
        (holds[0].name.as_str(), holds[0].placed_by),
        ("review", HoldPlacer::Agent)
    );
}

#[tokio::test]
async fn an_approval_decided_during_a_removal_places_no_hold() {
    let (temp, manager) = manager().await;
    let chat = chat(&manager, temp.path()).await;
    let Ok(crate::daemon::allocation::Allocation::Approval(requested)) =
        (manager.spaces.request_hold(
            &*manager,
            &SpaceSelector::id(&chat.id),
            "review".into(),
            Some("Keep it".into()),
        ))
        .await
    else {
        panic!("holding its own chat needs approval");
    };
    let removal = manager.spaces.space_removals.reserve(&chat.id).unwrap();
    manager
        .coordination
        .decide_approval(
            &*manager,
            requested.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(!manager.spaces.has_holds(&chat.id).await.unwrap());
    // The request stays undecided, so the decision can be made again.
    drop(removal);
    manager
        .coordination
        .decide_approval(
            &*manager,
            requested.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    assert!(manager.spaces.has_holds(&chat.id).await.unwrap());
}
