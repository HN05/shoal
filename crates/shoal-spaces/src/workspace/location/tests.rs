use crate::stand_in::Fixture;
use shoal_core::test_support::repository;

#[tokio::test]
async fn workspaces_stay_out_of_the_directories_shoal_keeps() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let directory = repo.workspaces_dir.clone();
    for reserved in [
        ".seed/inside",
        ".caches",
        ".tmp/id",
        ".attachments/id",
        ".sessions/feature",
    ] {
        let location = manager
            .spaces
            .workspace_location(&*manager, &repo, &directory.join(reserved))
            .await;
        assert!(location.is_err(), "{reserved}");
    }
    manager
        .spaces
        .workspace_location(&*manager, &repo, &directory.join("feature"))
        .await
        .unwrap();
}
