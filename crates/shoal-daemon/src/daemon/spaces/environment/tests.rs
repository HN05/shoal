use super::*;
use crate::{daemon::test_support, state::WorkspaceState};

#[tokio::test]
async fn a_workspace_environment_requires_a_ready_verified_worktree_and_carries_no_token() {
    let (root, manager) = test_support::manager().await;
    let repo = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "external".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let exported = manager.space_environment(&selector).await.unwrap();
    assert!(!exported.contains_key(env::SCOPE_TOKEN));
    manager
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    assert!(manager.space_environment(&selector).await.is_err());
    manager
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    std::fs::rename(
        workspace.path.join(".git"),
        workspace.path.join(".git.saved"),
    )
    .unwrap();
    assert!(manager.space_environment(&selector).await.is_err());
}
