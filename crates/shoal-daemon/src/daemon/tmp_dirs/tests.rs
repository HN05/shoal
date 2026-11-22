use shoal_ids::space_ids::SpaceId;
use std::os::unix::fs::PermissionsExt;

use crate::{
    daemon::{
        executions::ExecutionKind,
        test_support::{manager, repository},
    },
    env,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_workspace_has_its_own_tmpdir_until_it_is_removed() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let expected = repo.workspaces_dir.join(".tmp").join(&workspace.id);

    let exported = manager
        .space_environment(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(exported[env::TMPDIR], expected.to_str().unwrap());
    let mode = std::fs::metadata(&expected).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    std::fs::remove_dir(&expected).unwrap();
    let started = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    assert_eq!(started.plan.environment[env::TMPDIR], expected.as_os_str());
    assert!(expected.is_dir(), "a tracked execution creates it");
    let settings = manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    let hooks = (manager.hook_environment(&workspace, Some(workspace.repository()), &settings))
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
        .remove_selected(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!expected.exists());
}

#[tokio::test]
async fn env_overrides_the_workspace_tmpdir() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[env]\nTMPDIR = '/scratch'\n".into()))
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let exported = manager
        .space_environment(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(exported[env::TMPDIR], "/scratch");
}

#[tokio::test]
async fn the_sweep_deletes_temporary_directories_without_a_record() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let layout = crate::tmp_dirs::Layout::of(&repo);
    let kept = layout.prepare(&workspace.id).unwrap();
    let orphan = layout
        .prepare(&SpaceId::from("removed-workspace-id"))
        .unwrap();
    std::fs::write(orphan.join("scratch"), "garbage").unwrap();

    manager.sweep_tmp_dirs().await.unwrap();
    assert!(!orphan.exists());
    assert!(kept.is_dir());
    assert!(chat.part.tmp_dir.unwrap().is_dir());
}

#[tokio::test]
async fn a_directory_that_cannot_be_deleted_does_not_keep_the_others() {
    let root = tempfile::tempdir().unwrap();
    let layout = crate::tmp_dirs::Layout::new(root.path(), &"repo".into());
    let orphan = layout.prepare(&SpaceId::from("orphan")).unwrap();
    // A file where the first path needs a directory: it cannot be deleted.
    std::fs::write(layout.root().join("file"), "").unwrap();
    let ids = vec![SpaceId::from("file/stuck"), SpaceId::from("orphan")];

    let removed = super::remove(layout, ids).await.unwrap();
    assert!(!orphan.exists());
    assert_eq!(removed.deleted, [SpaceId::from("orphan")]);
    let error = removed.into_result().unwrap_err().to_string();
    assert!(error.contains("file/stuck"), "{error}");
}
