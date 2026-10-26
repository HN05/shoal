use super::*;
use crate::{
    daemon::scope,
    protocol::{ExecutionMethod, NotificationMethod, Operation, WorkspaceMethod},
    state::WorkspaceState,
    test_support,
};

#[tokio::test]
async fn exported_scopes_are_persistent_independent_and_untracked() {
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
    let first = manager
        .workspace_environment(&workspace.name)
        .await
        .unwrap();
    let second = manager.workspace_environment(&workspace.id).await.unwrap();
    let token = &first[env::SCOPE_TOKEN];
    assert_ne!(token, &second[env::SCOPE_TOKEN]);
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(!first.contains_key(env::EXECUTION_ID));
    let caller = manager.caller(token).await.unwrap().unwrap();
    assert_eq!(caller.workspace_id, workspace.id);
    assert!(caller.execution.is_none());
    for mut method in [
        Operation::from(WorkspaceMethod::WorkspaceEnv {
            workspace: workspace.id.clone(),
        }),
        WorkspaceMethod::RevokeWorkspaceEnv {
            workspace: workspace.id.clone(),
            token: token.clone(),
        }
        .into(),
        ExecutionMethod::CheckLanding.into(),
        NotificationMethod::ListNotifications {
            unread_only: true,
            limit: 50,
        }
        .into(),
    ] {
        assert!(
            scope::authorize(&manager, Some(token), &mut method)
                .await
                .is_err()
        );
    }
    manager.store.shutdown().await;
    let restored = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(restored.caller(token).await.unwrap().is_some());
    restored
        .revoke_workspace_environment(&workspace.id, token.clone())
        .await
        .unwrap();
    assert!(restored.caller(token).await.unwrap().is_none());
    assert!(
        restored
            .caller(&second[env::SCOPE_TOKEN])
            .await
            .unwrap()
            .is_some()
    );
    let id = workspace.id.clone();
    restored
        .store
        .run(move |db| {
            db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        restored
            .caller(&second[env::SCOPE_TOKEN])
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn exporting_requires_ready_verified_worktree_without_minting_on_failure() {
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
    manager
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    assert!(manager.workspace_environment(&workspace.id).await.is_err());
    manager
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    std::fs::rename(
        workspace.path.join(".git"),
        workspace.path.join(".git.saved"),
    )
    .unwrap();
    assert!(manager.workspace_environment(&workspace.id).await.is_err());
    assert_eq!(
        manager
            .store
            .run(|db| Ok(
                db.query_row("SELECT count(*) FROM workspace_scopes", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .await
            .unwrap(),
        0
    );
}
