use crate::model::EventCause;
use crate::stand_in::Fixture;
use crate::{
    events::{EventItem, EventKind},
    model::{CleanupReason, RefPart, SpaceRef, Workspace, WorkspaceRef},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_core::test_support::repository;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

fn space(workspace: &Workspace) -> SpaceRef {
    SpaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        part: RefPart::Workspace {
            branch: workspace.branch().cloned(),
        },
    }
}

fn reference(workspace: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch().cloned(),
    }
}

#[tokio::test]
async fn splitting_records_the_parent_and_removal_takes_the_subtree_first() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let root = manager
        .create_workspace(&repo.id, "root".into(), None, None, None)
        .await
        .unwrap();
    let middle = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&root.id),
            "middle".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // A base naming the parent's branch stacks the child on it too.
    let leaf = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&middle.id),
            "leaf".into(),
            Some("middle".into()),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(middle.parent, Some(space(&root)));
    assert_eq!(leaf.parent, Some(space(&middle)));
    assert_eq!(leaf.base_workspace, Some(reference(&middle)));
    assert_eq!(
        manager
            .spaces
            .workspace(&manager.named("root").await)
            .await
            .unwrap()
            .children,
        [space(&middle)]
    );
    assert_eq!(
        manager.spaces.subtree(&root.id).await.unwrap(),
        [root.id.clone(), middle.id.clone(), leaf.id.clone()]
    );
    assert!(
        manager
            .spaces
            .within_subtree(&root.id, &leaf.id)
            .await
            .unwrap()
    );
    assert!(
        !manager
            .spaces
            .within_subtree(&leaf.id, &root.id)
            .await
            .unwrap()
    );

    // Neither the leaf's hold nor its unsaved file keeps it; a checkpoint
    // keeps the file.
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&leaf.id),
            "editor".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    std::fs::write(leaf.path.join("draft.txt"), "draft").unwrap();
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&root.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert_eq!(removed.children, ["leaf", "middle"]);
    for workspace in [&root, &middle, &leaf] {
        assert!(
            manager
                .spaces
                .find_workspace(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace.path.exists());
    }
    let restorable: Vec<_> = manager
        .spaces
        .removed_workspaces()
        .await
        .unwrap()
        .into_iter()
        .map(|removed| removed.name)
        .collect();
    for name in ["root", "middle", "leaf"] {
        assert!(restorable.iter().any(|removed| removed == name), "{name}");
    }

    let events = manager.space_events(None, 100).await.unwrap();
    let removals: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::Removed => {
                Some((event.details.name.to_string(), event.details.cause))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        removals,
        [
            ("leaf".to_owned(), Some(EventCause::Parent)),
            ("middle".to_owned(), Some(EventCause::Parent)),
            ("root".to_owned(), Some(EventCause::Manual)),
        ]
    );
}

#[tokio::test]
async fn a_child_that_cannot_go_retains_its_parent() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
    manager
        .spaces
        .reserve_lifecycle(&child.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&parent.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    let kept = manager.spaces.workspace_by_id(&parent.id).await.unwrap();
    assert_eq!(kept.state, crate::state::WorkspaceState::Ready);
    assert!(kept.path.exists());
    assert_eq!(kept.children, [space(&child)]);
}

#[tokio::test]
async fn workspace_processes_list_their_subtree_with_their_own_workspace_first() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    create("sibling").await.unwrap();
    // The child sorts before its parent by name.
    manager
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
    let names = |spaces: Vec<crate::model::Space>| {
        spaces
            .into_iter()
            .map(|space| space.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(
            manager
                .coordination
                .visible_spaces(&*manager, None)
                .await
                .unwrap()
        ),
        ["child", "parent", "sibling"]
    );
    let caller = crate::scope::Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    assert_eq!(
        names(
            manager
                .coordination
                .visible_spaces(&*manager, Some(&caller))
                .await
                .unwrap()
        ),
        ["parent", "child"]
    );
}

#[tokio::test]
async fn children_never_keep_their_parent_and_go_with_it() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&child.id),
            "editor".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    std::fs::write(child.path.join("draft.txt"), "draft").unwrap();
    assert!(matches!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(parent.clone()))
            .await,
        CleanupReason::Idle { .. }
    ));
    assert_eq!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(child.clone()))
            .await,
        CleanupReason::Held
    );

    let snapshot = manager
        .spaces
        .cleanup_snapshot(&*manager, &parent.id)
        .await
        .unwrap()
        .unwrap();
    manager
        .spaces
        .remove_for_disk_space(&*manager, &SpaceSelector::id(&parent.id), snapshot)
        .await
        .unwrap();
    for workspace in [&parent, &child] {
        assert!(
            manager
                .spaces
                .find_workspace(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace.path.exists());
    }
    // The user hears of the child an unattended removal took.
    let notifications = manager.spaces.notifications.list(true, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.space.as_deref() == Some("child")),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_parent_must_be_a_space_of_the_same_repository() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let foreign = manager
        .create_workspace(&repos[1].id, "foreign".into(), None, None, None)
        .await
        .unwrap();
    let ensured = manager
        .store
        .run(move |db| {
            crate::tree::ensure_parent(db, Some(&repos[0].id), &SpaceId::from("new"), &foreign.id)
        })
        .await;
    assert!(ensured.is_err());
}

#[tokio::test]
async fn a_parent_reserved_for_removal_refuses_splits() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .reserve_lifecycle(&parent.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    let split = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await;
    assert!(split.is_err());
    assert!(
        manager
            .spaces
            .workspace(&manager.named("child").await)
            .await
            .is_err()
    );
    assert!(
        manager
            .spaces
            .workspace_by_id(&parent.id)
            .await
            .unwrap()
            .children
            .is_empty()
    );
}

#[tokio::test]
async fn a_launch_reopens_only_its_recorded_workspace() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
    let opened = manager
        .spaces
        .reopen_workspace(&*manager, &SpaceSelector::id(&child.id))
        .await
        .unwrap();
    assert!(opened.reused && opened.workspace.id == child.id);
    // Removed during the handoff, the child is not created again from its branch.
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&child.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .reopen_workspace(&*manager, &SpaceSelector::id(&child.id))
            .await
            .is_err()
    );
    assert_eq!(manager.spaces.list_workspaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_removal_takes_only_the_children_it_was_confirmed_with() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
    let check = manager
        .spaces
        .check_manual_removal(&*manager, &SpaceSelector::id(&parent.id), false)
        .await
        .unwrap();
    assert_eq!(check.children, [space(&child)]);
    let confirmed = UnsavedWork::confirming(&check);
    // Splits after the confirmation, directly or below a confirmed child,
    // keep the whole tree.
    let late = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&child.id),
            "late".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&parent.id),
            confirmed.clone(),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    for workspace in [&parent, &child, &late] {
        assert!(
            manager
                .spaces
                .find_workspace(&workspace.id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(workspace.path.exists());
    }
    // Confirmed again, the removal takes them all.
    let check = manager
        .spaces
        .check_manual_removal(&*manager, &SpaceSelector::id(&parent.id), false)
        .await
        .unwrap();
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&parent.id),
            UnsavedWork::confirming(&check),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert_eq!(removed.children, ["late", "child"]);
}

#[tokio::test]
async fn a_workspace_keeps_the_parent_its_creation_recorded_on_another_machine() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "child".into(), None, None, None)
        .await
        .unwrap();
    // A parent this machine does not have, as a creation records one the
    // hub lists on another machine.
    let elsewhere = SpaceId::generate();
    let (id, parent) = (workspace.id.clone(), elsewhere.clone());
    (manager.store)
        .write(move |tx| crate::tree::set_parent(tx, &id, Some(&parent)))
        .await
        .unwrap();
    (manager
        .spaces
        .assign_parent(&*manager, &workspace.id, Some(elsewhere.clone())))
    .await
    .unwrap();
    let id = workspace.id.clone();
    let parent = (manager.store)
        .read(move |tx| crate::tree::parent_id(tx, &id))
        .await
        .unwrap();
    assert_eq!(parent, Some(elsewhere));
}
