use crate::model::Repository;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;

use shoal_protocol::space_names::SpaceSelector;

/// A registered repository with the ready workspace `worker`.
async fn workspace(root: &std::path::Path, manager: &Fixture) -> (Repository, Workspace) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    (repo, workspace)
}
/// A chat runs the diff itself in the path the diff base names, so the base
/// names only the workspace's own worktree.
#[tokio::test]
async fn the_diff_base_names_only_the_workspaces_own_worktree() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let (repo, workspace) = workspace(temp.path(), &manager).await;
    let base = manager
        .spaces
        .diff_base(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(base.path, workspace.path);
    // A clone at its path has the same history, so only the ownership check
    // tells it apart.
    std::fs::rename(&workspace.path, temp.path().join("moved")).unwrap();
    let clone = [repo.path.as_os_str(), workspace.path.as_os_str()].map(|p| p.to_str().unwrap());
    crate::stand_in::git(temp.path(), &["clone", "-q", clone[0], clone[1]]);
    crate::stand_in::git(&workspace.path, &["checkout", "-q", "-b", "worker"]);
    assert!(
        manager
            .spaces
            .diff_base(&*manager, &SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
}
