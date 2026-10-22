use crate::{
    daemon::{
        events::{EventCause, EventItem, EventKind},
        scope::{Caller, ScopedExecution},
    },
    model::{CleanupReason, Workspace, WorkspaceRef},
    protocol::Method,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{manager, repository},
};

fn reference(workspace: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

#[tokio::test]
async fn splitting_records_the_parent_and_removal_moves_children_up() {
    let (temp, manager) = manager().await;
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
        .split_workspace(&root.name, "middle".into(), None)
        .await
        .unwrap();
    // A base naming the parent's branch stacks the child on it too.
    let leaf = manager
        .split_workspace(&middle.id, "leaf".into(), Some("middle".into()))
        .await
        .unwrap();
    assert_eq!(middle.parent_workspace, Some(reference(&root)));
    assert_eq!(leaf.parent_workspace, Some(reference(&middle)));
    assert_eq!(leaf.base_workspace, Some(reference(&middle)));
    assert_eq!(
        manager.workspace("root").await.unwrap().child_workspaces,
        [reference(&middle)]
    );
    assert_eq!(
        manager.subtree(&root.id).await.unwrap(),
        [root.id.clone(), middle.id.clone(), leaf.id.clone()]
    );
    assert!(manager.within_subtree(&root.id, &leaf.id).await.unwrap());
    assert!(!manager.within_subtree(&leaf.id, &root.id).await.unwrap());

    for (removed, parent) in [(&middle, Some(reference(&root))), (&root, None)] {
        manager
            .remove_workspace(
                &removed.id,
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.workspace("leaf").await.unwrap().parent_workspace,
            parent
        );
    }

    let events = manager.workspace_events(None, 100).await.unwrap();
    let parents: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event)
                if event.details.workspace_id == leaf.id
                    && matches!(
                        event.details.kind,
                        EventKind::Created | EventKind::ParentChanged
                    ) =>
            {
                Some((
                    event.details.kind,
                    event.details.cause,
                    event.details.parent_workspace.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        parents,
        [
            (EventKind::Created, None, Some(Some(reference(&middle)))),
            (
                EventKind::ParentChanged,
                Some(EventCause::Removed),
                Some(Some(reference(&root)))
            ),
            (
                EventKind::ParentChanged,
                Some(EventCause::Removed),
                Some(None)
            ),
        ]
    );
}

#[tokio::test]
async fn workspace_processes_reach_their_subtree_and_split_within_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: child.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: super::super::ExecutionKind::Command,
                }),
            },
        )
        .await;
    let split = |workspace: &str| Method::SplitWorkspace {
        workspace: workspace.into(),
        name: "grandchild".into(),
        base: None,
    };
    for (target, allowed) in [
        (&child.name, true),
        (&parent.name, false),
        (&sibling.name, false),
    ] {
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut split(target)).await;
        assert_eq!(authorized.is_ok(), allowed, "{target}");
    }
    let grandchild = manager
        .split_workspace(&child.id, "grandchild".into(), None)
        .await
        .unwrap();
    let mut done = Method::WorkspaceDone {
        workspace: grandchild.name.clone(),
        cleanup: false,
    };
    crate::daemon::scope::authorize(&manager, Some("scope"), &mut done)
        .await
        .unwrap();
    // Targets resolve to the named workspace, not the caller's own.
    assert!(
        matches!(&done, Method::WorkspaceDone { workspace, .. } if *workspace == grandchild.id)
    );
}

#[tokio::test]
async fn children_keep_their_parent_from_cleanup() {
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
    assert_eq!(
        manager.cleanup_reason(&parent).await,
        CleanupReason::Children
    );
    assert_eq!(
        manager.cleanup_reason(&child).await,
        CleanupReason::Idle { minutes: 10 }
    );
    manager
        .remove_workspace(&child.id, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&parent).await,
        CleanupReason::Idle { minutes: 10 }
    );
}

#[tokio::test]
async fn a_parent_must_be_a_workspace_of_the_same_repository() {
    let (temp, manager) = manager().await;
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
    let error = manager
        .store
        .run(move |db| super::ensure_parent(db, &repos[0].id, &foreign.id))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("not a workspace of this repository"));
}

#[tokio::test]
async fn a_parent_reserved_for_removal_refuses_splits() {
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
    manager
        .reserve_lifecycle(&parent.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    let error = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("parent workspace is removing"),
        "{error:#}"
    );
    assert!(manager.workspace("child").await.is_err());
    assert!(
        manager
            .workspace(&parent.id)
            .await
            .unwrap()
            .child_workspaces
            .is_empty()
    );
}
