use crate::test_support::{manager, repository};

#[tokio::test]
async fn a_contribution_clones_a_url_and_keeps_an_existing_registration() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let source = path.to_str().unwrap().to_owned();
    let error = manager
        .register_contribution(source.clone(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("local path"), "{error:#}");
    let registered = manager
        .register_repository(source.as_str().into(), None, None)
        .await
        .unwrap();
    let again = manager.register_contribution(source, None).await.unwrap();
    assert_eq!(again.id, registered.id);
    assert!(!again.contribution);
}
