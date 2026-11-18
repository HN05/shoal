use super::*;
use crate::daemon::test_support;
use crate::daemon::workspace::creation::WorkspaceSource;
use shoal_ids::space_ids::SpaceId;

#[tokio::test]
async fn removal_waits_for_queued_workspace_creation() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();

    let gate = manager.gates.lock_git(&repo.id).await;
    let creating_manager = manager.clone();
    let creating_repo = repo.clone();
    let mut creating = Box::pin(async move {
        let git = creating_manager.gates.lock_git(&creating_repo.id).await;
        creating_manager
            .create_branch_workspace(
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
    let mut removing = Box::pin(manager.remove_repository(&repo.id));
    test_support::assert_pending(&mut removing).await;
    drop(gate);

    let workspace = creating.await.unwrap();
    let removal = removing.await.unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(!workspace.path.exists());
    assert!(manager.repositories().await.unwrap().is_empty());
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
    assert!(!repo.workspaces_dir.exists());
}

#[tokio::test]
async fn removal_takes_a_workspace_its_preparation_has_yet_to_create() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = crate::daemon::workspace::NewWorkspace {
        pending: true,
        ..Default::default()
    };
    let workspace = manager
        .create_workspace_with(&repo.id, "racing".into(), None, new)
        .await
        .unwrap();
    assert_eq!(workspace.state, WorkspaceState::Preparing);

    let removal = manager.remove_repository(&repo.id).await.unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
    // The preparation, which waited for the removal, finds nothing to create.
    let selector = SpaceSelector::id(&workspace.id);
    assert!(manager.create_pending_worktree(&selector).await.is_err());
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn removal_takes_a_child_being_prepared_below_a_parent_listed_first() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace_with(&repo.id, "a-parent".into(), None, Default::default())
        .await
        .unwrap();
    let new = crate::daemon::workspace::NewWorkspace {
        pending: true,
        parent: Some(parent.id.clone()),
        ..Default::default()
    };
    let child = manager
        .create_workspace_with(&repo.id, "b-child".into(), None, new)
        .await
        .unwrap();
    assert_eq!(child.state, WorkspaceState::Preparing);

    let removal = manager.remove_repository(&repo.id).await.unwrap();
    assert_eq!(removal.workspaces_removed, 2);
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(!parent.path.exists());
    assert!(!repo.path.exists());
}

#[tokio::test]
async fn removal_takes_a_child_being_prepared_below_a_chat_in_a_workspace_listed_first() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let top = manager
        .create_workspace_with(&repo.id, "a-top".into(), None, Default::default())
        .await
        .unwrap();
    let below_top = crate::daemon::workspace::NewWorkspace {
        parent: Some(top.id.clone()),
        ..Default::default()
    };
    let host = manager
        .create_workspace_with(&repo.id, "b-host".into(), None, below_top)
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
        .create_workspace_with(&repo.id, "c-child".into(), None, below_chat)
        .await
        .unwrap();
    assert_eq!(child.state, WorkspaceState::Preparing);

    let removal = manager.remove_repository(&repo.id).await.unwrap();
    assert_eq!(removal.workspaces_removed, 3);
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
}

#[tokio::test]
async fn removal_takes_a_chat_in_the_checkout_with_the_workspace_below_it() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
        .create_workspace_with(&repo.id, "child".into(), None, below_chat)
        .await
        .unwrap();

    let removal = manager.remove_repository(&repo.id).await.unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(manager.chats().await.unwrap().is_empty());
    // The removal journals each space, so a request for it names the
    // repository it went with.
    let repository = format!("removed with its repository {}", repo.id);
    for id in [&chat.id, &child.id] {
        let refused = manager.space_by_id(id).await.unwrap_err().to_string();
        assert!(refused.contains(&repository), "{refused}");
    }
}

#[tokio::test]
async fn removal_refuses_a_checkout_whose_config_sets_bare() -> Result<()> {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    test_support::git(&repo.path, &["config", "core.bare", "true"]);

    assert!(manager.remove_repository(&repo.id).await.is_err());
    assert!(repo.path.exists());

    test_support::git(&repo.path, &["config", "core.bare", "false"]);
    manager.remove_repository(&repo.id).await?;
    assert!(!repo.path.exists());
    Ok(())
}

#[test]
fn recorded_identity_verifies_and_refuses_a_replaced_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let directory = root.join("checkout");
    fs::create_dir(&directory)?;
    let current = real_directory_identity(&directory)?.context("missing identity")?;
    verify_directory(&directory, Some(&current), false)?;
    assert!(verify_directory(&directory, None, true).is_err());

    let moved = root.join("moved");
    fs::rename(&directory, &moved)?;
    fs::create_dir(&directory)?;
    assert!(verify_directory(&directory, Some(&current), true).is_err());
    fs::remove_dir(&directory)?;
    verify_directory(&directory, Some(&current), true)?;
    verify_directory(&directory, None, false)?;
    assert!(verify_directory(&directory, Some(&current), false).is_err());
    Ok(())
}

#[tokio::test]
async fn the_local_config_goes_only_when_it_is_the_repositorys_own_directory() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("config.toml"), "").unwrap();
    let linked = root.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join(".shoal")).unwrap();
    delete_local_config(linked.clone()).await.unwrap();
    assert!(elsewhere.join("config.toml").exists());
    // A redirected ancestor leads elsewhere too.
    let target = root.path().join("target");
    std::fs::create_dir_all(target.join("repo/.shoal")).unwrap();
    std::fs::write(target.join("repo/.shoal/config.toml"), "").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("redirect")).unwrap();
    delete_local_config(root.path().join("redirect/repo"))
        .await
        .unwrap();
    assert!(target.join("repo/.shoal/config.toml").exists());
    // A Git checkout there holds tracked files.
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    let checkout = canonical.join("checkout");
    std::fs::create_dir_all(checkout.join(".shoal")).unwrap();
    std::fs::write(checkout.join(".shoal/.git"), "gitdir: elsewhere\n").unwrap();
    delete_local_config(checkout.clone()).await.unwrap();
    assert!(checkout.join(".shoal/.git").exists());

    let owned = canonical.join("owned");
    std::fs::create_dir_all(owned.join(".shoal")).unwrap();
    std::fs::write(owned.join(".shoal/config.toml"), "").unwrap();
    delete_local_config(owned.clone()).await.unwrap();
    assert!(!owned.join(".shoal").exists());
}

#[tokio::test]
async fn removal_refuses_a_repository_inside_its_local_config_directory() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "parent");
    let parent = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager.local_config_path(&parent.id).await.unwrap();
    let directory = local.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    // Registration refuses this place; an earlier version recorded it.
    let elsewhere = test_support::repository(root.path(), "nested");
    let registered = manager
        .register_repository(elsewhere.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let nested = directory.join("nested");
    manager
        .move_checkout(&registered.id, &nested)
        .await
        .unwrap();
    assert!(manager.remove_repository(&parent.id).await.is_err());
    assert!(nested.exists());
}

#[tokio::test]
async fn removal_refuses_a_checkout_holding_another_repositorys_directory() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "project");
    let elsewhere = test_support::repository(root.path(), "elsewhere");
    let other = manager
        .register_repository(elsewhere.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // An earlier root_dir placed the other repository's directory here.
    let reserved = std::fs::canonicalize(&checkout)
        .unwrap()
        .join(".shoal")
        .join("elsewhere");
    std::fs::create_dir_all(reserved.join(".shoal")).unwrap();
    let (id, directory) = (other.id.clone(), reserved.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE repositories SET workspaces_dir=?2 WHERE id=?1",
                rusqlite::params![id, directory.to_str()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let project = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert!(manager.remove_repository(&project.id).await.is_err());
    assert!(reserved.join(".shoal").exists());
}
