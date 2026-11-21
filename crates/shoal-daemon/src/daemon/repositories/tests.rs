mod configuration;

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
