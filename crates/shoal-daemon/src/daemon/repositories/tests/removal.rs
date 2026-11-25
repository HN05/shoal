use crate::daemon::test_support;
use crate::daemon::workspace::creation::WorkspaceSource;
use crate::state::WorkspaceState;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn removal_waits_for_queued_workspace_creation() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();

    let gate = manager.gates.lock_git(&repo.id).await;
    let creating_manager = manager.clone();
    let creating_repo = repo.clone();
    let mut creating = Box::pin(async move {
        let git = creating_manager.gates.lock_git(&creating_repo.id).await;
        creating_manager
            .spaces
            .create_branch_workspace(
                &*creating_manager,
                &creating_repo,
                "racing".into(),
                "racing".into(),
                WorkspaceSource::New {
                    base: None,
                    parent: None,
                },
                Default::default(),
                git,
            )
            .await
    });
    test_support::assert_pending(&mut creating).await;

    // Shared caches and temporary directories go with the repository directory.
    let caches = std::collections::BTreeMap::from([(
        "go".to_owned(),
        crate::config::repo::Cache {
            scope: crate::config::repo::CacheScope::Shared,
        },
    )]);
    crate::caches::directories(&repo, &caches).unwrap();
    crate::tmp_dirs::directory(&repo, &SpaceId::from("chat-id")).unwrap();
    let mut removing = Box::pin(manager.repositories.remove(&*manager, &repo.id));
    test_support::assert_pending(&mut removing).await;
    drop(gate);

    let workspace = creating.await.unwrap();
    let removal = removing.await.unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(!workspace.path.exists());
    assert!(manager.repositories.list().await.unwrap().is_empty());
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
    assert!(!repo.workspaces_dir.exists());
}

#[tokio::test]
async fn removal_takes_a_workspace_its_preparation_has_yet_to_create() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = crate::daemon::workspace::NewWorkspace {
        pending: true,
        ..Default::default()
    };
    let workspace = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "racing".into(), None, new)
        .await
        .unwrap();
    assert_eq!(workspace.state, WorkspaceState::Preparing);

    let removal = manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
    // The preparation, which waited for the removal, finds nothing to create.
    let selector = SpaceSelector::id(&workspace.id);
    assert!(
        manager
            .spaces
            .create_pending_worktree(&*manager, &selector)
            .await
            .is_err()
    );
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn removal_takes_a_child_being_prepared_below_a_parent_listed_first() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .spaces
        .create_workspace_with(
            &*manager,
            &repo.id,
            "a-parent".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let new = crate::daemon::workspace::NewWorkspace {
        pending: true,
        parent: Some(parent.id.clone()),
        ..Default::default()
    };
    let child = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "b-child".into(), None, new)
        .await
        .unwrap();
    assert_eq!(child.state, WorkspaceState::Preparing);

    let removal = manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert_eq!(removal.workspaces_removed, 2);
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
    assert!(!parent.path.exists());
    assert!(!repo.path.exists());
}

#[tokio::test]
async fn removal_takes_a_child_being_prepared_below_a_chat_in_a_workspace_listed_first() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let top = manager
        .spaces
        .create_workspace_with(
            &*manager,
            &repo.id,
            "a-top".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let below_top = crate::daemon::workspace::NewWorkspace {
        parent: Some(top.id.clone()),
        ..Default::default()
    };
    let host = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "b-host".into(), None, below_top)
        .await
        .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&host.id),
            "watching".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let below_chat = crate::daemon::workspace::NewWorkspace {
        pending: true,
        parent: Some(chat.id.clone()),
        ..Default::default()
    };
    let child = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "c-child".into(), None, below_chat)
        .await
        .unwrap();
    assert_eq!(child.state, WorkspaceState::Preparing);

    let removal = manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert_eq!(removal.workspaces_removed, 3);
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
}

#[tokio::test]
async fn removal_takes_a_chat_in_the_checkout_with_the_workspace_below_it() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let below_chat = crate::daemon::workspace::NewWorkspace {
        parent: Some(chat.id.clone()),
        ..Default::default()
    };
    let child = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "child".into(), None, below_chat)
        .await
        .unwrap();

    let removal = manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(manager.spaces.chats().await.unwrap().is_empty());
    // The removal journals each space, so a request for it names the
    // repository it went with.
    let repository = format!("removed with its repository {}", repo.id);
    for id in [&chat.id, &child.id] {
        let refused = manager
            .spaces
            .space_by_id(id)
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains(&repository), "{refused}");
    }
}
