use crate::model::EventCause;
use crate::stand_in::Fixture;
use crate::{
    events::{EventItem, EventKind},
    model::{CleanupReason, RefPart, SpaceRef, Workspace, WorkspaceRef},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_core::test_support::repository;
use shoal_daemon_base::callers::Access;
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
        access: Access::Full,
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

/// Register a repository for each of `names`.
async fn repositories(manager: &Fixture, names: &[&str]) -> Vec<crate::model::Repository> {
    let mut repos = Vec::new();
    for name in names {
        repos.push(manager.repository(name).await);
    }
    repos
}

/// Record the chat `name` without a repository, below `parent` when given,
/// with its agent running as the returned execution.
async fn outside(
    manager: &Fixture,
    name: &str,
    parent: Option<&SpaceId>,
) -> (crate::model::Chat, shoal_ids::execution_ids::ExecutionId) {
    let chat = manager.chat(name, "orchestrate", None, parent).await;
    let agent = manager.run_agent(&chat.id).await;
    (chat, agent)
}

/// Create the workspace `name` of `repository` below `parent` when given,
/// with a working agent.
async fn worked(
    manager: &Fixture,
    repository: &crate::model::Repository,
    name: &str,
    parent: Option<&SpaceId>,
) -> Workspace {
    let workspace = manager
        .spaces
        .create_workspace_below(
            &**manager,
            repository.id.clone().into(),
            name.into(),
            None,
            Default::default(),
            parent.cloned(),
        )
        .await
        .unwrap();
    manager.run_agent(&workspace.id).await;
    manager.report(&workspace.id, crate::state::AgentState::Working);
    workspace
}

#[tokio::test]
async fn a_chat_without_a_repository_keeps_its_temporary_directory_in_root_dir() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let (master, _agent) = outside(&manager, "master", None).await;
    assert_eq!(master.repository_id, None);
    let tmp_dir = master.tmp_dir.clone().unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    let root_dir = manager.config.current().root_dir(&manager.paths).unwrap();
    assert_eq!(tmp_dir, root_dir.join(".tmp").join(&master.id));
    let listed = manager
        .spaces
        .chat_by_id(&master.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    // A daemon with another state directory sharing root_dir neither lists
    // nor takes it.
    let other =
        shoal_core::tmp_dirs::Layout::outside_repositories(&root_dir, &temp.path().join("other"));
    assert_eq!(other.list().unwrap(), Vec::<String>::new());
    other.prepare(&SpaceId::from("another")).unwrap_err();
    manager.spaces.sweep_tmp_dirs(&*manager).await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    // A new root_dir leaves it where the chat started, as a repository
    // keeps its directory.
    let moved = std::fs::canonicalize(temp.path()).unwrap().join("moved");
    crate::stand_in::configure(&manager, &format!("[machine]\nroot_dir = {moved:?}\n")).await;
    let listed = manager
        .spaces
        .chat_by_id(&master.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    assert_eq!(listed.workdir.root_dir(), Some(root_dir.as_path()));
    let directory = manager
        .spaces
        .chat_checkout_path(&*manager, &listed)
        .await
        .unwrap();
    assert_eq!(directory, root_dir);
    let space = crate::model::Space::from(listed);
    let space = (manager
        .spaces
        .space_ties(
            &*manager,
            &space,
            crate::space_processes::WorktreeTies::None,
        )
        .await)
        .unwrap();
    assert_eq!(space.directories, std::slice::from_ref(&tmp_dir));
    manager.spaces.sweep_tmp_dirs(&*manager).await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&master.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!tmp_dir.exists(), "{}", tmp_dir.display());
}

#[tokio::test]
async fn a_chat_without_a_repository_takes_children_in_every_repository() {
    let manager = Fixture::new().await;
    let repos = repositories(&manager, &["repo", "other"]).await;
    let (master, _agent) = outside(&manager, "master", None).await;
    let first = worked(&manager, &repos[0], "first", Some(&master.id)).await;
    let second = worked(&manager, &repos[1], "second", Some(&master.id)).await;
    for workspace in [&first, &second] {
        let parent = manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent;
        assert_eq!(parent.map(|parent| parent.id), Some(master.id.clone()));
    }
    // A chat without a repository goes below another one.
    let (nested, _nested_agent) = outside(&manager, "nested", Some(&master.id)).await;
    assert_eq!(nested.parent.map(|parent| parent.id), Some(master.id));
    // Every other parent keeps its children in its repository.
    let foreign = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repos[1].id.clone().into(),
            "foreign".into(),
            None,
            Default::default(),
            Some(first.id.clone()),
        )
        .await;
    assert!(foreign.is_err());
    let below_workspace = crate::chats::NewChat {
        priority: None,
        narrowing: Default::default(),
        name: "below".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: Some(first.id.clone()),
    };
    let refused = manager
        .spaces
        .start_new_chat(&*manager, None, None, below_workspace)
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn removing_a_chat_without_a_repository_removes_its_children_in_every_repository() {
    let manager = Fixture::new().await;
    let repos = repositories(&manager, &["repo", "other"]).await;
    let (master, agent) = outside(&manager, "master", None).await;
    let first = worked(&manager, &repos[0], "first", Some(&master.id)).await;
    let second = worked(&manager, &repos[1], "second", Some(&master.id)).await;
    manager.end_agent(&agent).await;
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&master.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let mut children: Vec<_> = removed
        .children
        .iter()
        .map(shoal_protocol::space_names::SpaceName::as_str)
        .collect();
    children.sort_unstable();
    assert_eq!(children, ["first", "second"]);
    assert!(
        manager
            .spaces
            .chat_by_id(&master.id)
            .await
            .unwrap()
            .is_none()
    );
    for workspace in [&first, &second] {
        let found = manager.spaces.find_workspace(&workspace.id).await.unwrap();
        assert!(found.is_none(), "{}", workspace.name);
    }
}
