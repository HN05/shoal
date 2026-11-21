use crate::daemon::test_support::repository;
use shoal_repositories::{root, spotlight::Layout};

#[tokio::test]
async fn an_excluded_root_refuses_a_workspace_at_its_excluded_directory() {
    let (temp, manager) = crate::daemon::test_support::indexed_manager().await;
    let origin = repository(temp.path(), "origin");
    let repo = manager
        .repositories
        .register(format!("file://{}", origin.display()).into(), None, None)
        .await
        .unwrap();
    let home = std::fs::canonicalize(temp.path()).unwrap();
    let placed = home.join(".other.noindex");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed))
        .await
        .unwrap();
    let root = root::ConfiguredRoot {
        path: home.join("other"),
        layout: Layout::Excluded,
    };
    let repositories = manager.repositories.list().await.unwrap();
    let workspaces = manager.list_workspaces().await.unwrap();
    let recorded = root::Recorded {
        repositories: &repositories,
        workspaces: &workspaces,
    };
    let prepared = root::prepare_root(&root, &home.join("state"), &recorded, None).await;
    assert!(prepared.is_err());
    assert!(std::fs::symlink_metadata(&root.path).is_err());
}

#[tokio::test]
async fn a_checkout_where_the_local_config_belongs_gets_its_own_directory() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let root = crate::config::Config::default()
        .root_dir(&manager.paths)
        .unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    let checkout = crate::daemon::test_support::repository(&root.join("project"), ".shoal");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager.local_config_path(&repo.id).await.unwrap();
    let checkout = std::fs::canonicalize(&checkout).unwrap();
    assert!(!local.starts_with(&checkout), "{}", local.display());
}

#[tokio::test]
async fn no_checkout_registers_where_another_repositorys_local_config_belongs() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(root.path(), "project");
    let project = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager.local_config_path(&project.id).await.unwrap();
    let directory = local.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    let other = crate::daemon::test_support::repository(directory, "other");
    let registered = manager
        .repositories
        .register(other.to_str().unwrap().into(), None, None)
        .await;
    assert!(registered.is_err());
    let cloned = manager
        .repositories
        .register(
            "https://example.test/team/other.git".into(),
            None,
            Some(directory.join("clone")),
        )
        .await;
    assert!(cloned.is_err());
    assert!(!directory.join("clone").exists());
}
