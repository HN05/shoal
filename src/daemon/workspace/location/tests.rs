use crate::test_support::{manager, repository};

#[tokio::test]
async fn workspaces_stay_out_of_the_seed_cache_and_temporary_directories() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let directory = repo.workspaces_dir.clone().unwrap();
    for reserved in [".seed/inside", ".caches", ".tmp/id"] {
        let location = manager
            .workspace_location(&repo, &directory.join(reserved))
            .await;
        assert!(location.is_err(), "{reserved}");
    }
    manager
        .workspace_location(&repo, &directory.join("feature"))
        .await
        .unwrap();
}
