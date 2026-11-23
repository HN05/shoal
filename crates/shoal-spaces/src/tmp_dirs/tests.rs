use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use shoal_ids::space_ids::SpaceId;

use crate::env;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn env_overrides_the_workspace_tmpdir() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
        .spaces
        .space_environment(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(exported[env::TMPDIR], "/scratch");
}

#[tokio::test]
async fn a_directory_that_cannot_be_deleted_does_not_keep_the_others() {
    let root = tempfile::tempdir().unwrap();
    let layout = crate::tmp_dirs::Layout::new(root.path(), &"repo".into());
    let orphan = layout.prepare(&SpaceId::from("orphan")).unwrap();
    // A file where the first path needs a directory: it cannot be deleted.
    std::fs::write(layout.root().join("file"), "").unwrap();
    let ids = vec![SpaceId::from("file/stuck"), SpaceId::from("orphan")];

    let removed = crate::tmp_dirs::remove(layout, ids).await.unwrap();
    assert!(!orphan.exists());
    assert_eq!(removed.deleted, [SpaceId::from("orphan")]);
    let error = removed.into_result().unwrap_err().to_string();
    assert!(error.contains("file/stuck"), "{error}");
}
