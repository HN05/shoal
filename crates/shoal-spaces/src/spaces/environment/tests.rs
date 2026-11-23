use crate::env;
use crate::stand_in::Fixture;
use shoal_protocol::space_names::SpaceSelector;

use crate::state::WorkspaceState;
use shoal_core::test_support::repository;

#[tokio::test]
async fn a_workspace_environment_requires_a_ready_verified_worktree_and_carries_no_token() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "external".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let exported = manager
        .spaces
        .space_environment(&*manager, &selector)
        .await
        .unwrap();
    assert!(!exported.contains_key(env::SCOPE_TOKEN));
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .space_environment(&*manager, &selector)
            .await
            .is_err()
    );
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    std::fs::rename(
        workspace.path.join(".git"),
        workspace.path.join(".git.saved"),
    )
    .unwrap();
    assert!(
        manager
            .spaces
            .space_environment(&*manager, &selector)
            .await
            .is_err()
    );
}
