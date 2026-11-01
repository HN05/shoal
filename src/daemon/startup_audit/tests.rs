use super::*;
use crate::test_support::{manager, repository};

#[tokio::test]
async fn recheck_readies_only_workspaces_the_audit_failed_once_they_verify() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for name in ["mounted", "setup", "missing"] {
        workspaces.push(
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap(),
        );
    }
    let failure = format!("{STARTUP_OWNERSHIP_FAILURE}: worktree was not there");
    for (workspace, error) in [
        (&workspaces[0], failure.clone()),
        (&workspaces[1], "setup failed (exit 1)".to_owned()),
        (&workspaces[2], failure.clone()),
    ] {
        manager
            .set_state(&workspace.id, WorkspaceState::Failed, Some(error))
            .await
            .unwrap();
    }
    // Its worktree is still gone, so its ownership cannot verify.
    std::fs::remove_dir_all(&workspaces[2].path).unwrap();

    manager.recheck_startup_failures().await.unwrap();

    let state = |id: String| {
        let manager = manager.clone();
        async move { manager.workspace(&id).await.unwrap() }
    };
    let mounted = state(workspaces[0].id.clone()).await;
    assert_eq!(mounted.state, WorkspaceState::Ready);
    assert_eq!(mounted.error, None);
    let setup = state(workspaces[1].id.clone()).await;
    assert_eq!(setup.state, WorkspaceState::Failed);
    assert_eq!(setup.error.as_deref(), Some("setup failed (exit 1)"));
    let missing = state(workspaces[2].id.clone()).await;
    assert_eq!(missing.state, WorkspaceState::Failed);
    assert_eq!(missing.error, Some(failure));
}

#[tokio::test]
async fn the_audit_fails_workspaces_whose_file_access_stalls_until_a_recheck_finishes() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await
        .unwrap();
    // Paused only around the stall, since real Git would let the clock
    // run out its time limit.
    tokio::time::pause();
    let stalled = manager
        .check_access(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    manager.audit_worktrees().await.unwrap();
    let audited = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(audited.state, WorkspaceState::Failed);
    let error = audited.error.unwrap();
    assert!(
        error.starts_with(STARTUP_OWNERSHIP_FAILURE)
            && error.contains("stalled; a macOS privacy prompt may be waiting"),
        "{error}"
    );

    // The recheck skips the directory until it is due to check it again.
    manager.recheck_startup_failures().await.unwrap();
    let skipped = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(skipped.state, WorkspaceState::Failed);
    tokio::time::pause();
    tokio::time::advance(crate::daemon::file_access::RECHECK_AFTER).await;
    tokio::time::resume();
    manager.recheck_startup_failures().await.unwrap();
    let ready = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(ready.state, WorkspaceState::Ready);
    assert_eq!(manager.file_access.stalls(), []);
}
