use std::os::unix::fs::PermissionsExt;

use crate::{
    daemon::workspace::ExecutionKind,
    env,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{manager, repository},
};

#[tokio::test]
async fn a_workspace_has_its_own_tmpdir_until_it_is_removed() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let expected = repo
        .workspaces_dir
        .unwrap()
        .join(".tmp")
        .join(&workspace.id);

    let exported = manager.workspace_environment(&workspace.id).await.unwrap();
    assert_eq!(exported[env::TMPDIR], expected.to_str().unwrap());
    let mode = std::fs::metadata(&expected).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    std::fs::remove_dir(&expected).unwrap();
    let started = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    assert_eq!(started.plan.environment[env::TMPDIR], expected.as_os_str());
    assert!(expected.is_dir(), "a tracked execution creates it");
    let hooks = manager
        .configured_workspace_environment(&workspace)
        .await
        .unwrap();
    assert_eq!(hooks[env::TMPDIR], expected.as_os_str());
    let removed = manager
        .removed_workspace_environment(&workspace)
        .await
        .unwrap();
    assert!(
        !removed.contains_key(env::TMPDIR),
        "post-remove inherits it"
    );
    manager
        .finish_execution(started.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();

    std::fs::write(expected.join("scratch"), "garbage").unwrap();
    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(!expected.exists());
}

#[tokio::test]
async fn env_overrides_the_workspace_tmpdir() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .set_repository_config(&repo.id, Some("[env]\nTMPDIR = '/scratch'\n".into()))
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let exported = manager.workspace_environment(&workspace.id).await.unwrap();
    assert_eq!(exported[env::TMPDIR], "/scratch");
}

#[tokio::test]
async fn a_tracked_execution_reserves_a_missing_repository_directory() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE repositories SET workspaces_dir=NULL WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let started = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let directory = manager
        .repository(&repo.id)
        .await
        .unwrap()
        .workspaces_dir
        .unwrap();
    let expected = directory.join(".tmp").join(&workspace.id);
    assert_eq!(started.plan.environment[env::TMPDIR], expected.as_os_str());
    assert!(expected.is_dir());
}
