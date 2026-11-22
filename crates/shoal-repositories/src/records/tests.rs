use shoal_core::test_support::{git, repository};

use crate::stand_in::Fixture;

#[tokio::test]
async fn a_repository_is_shared_under_its_origin_s_url_and_only_with_one() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let path = repository(fixture.root.path(), "repo");
    let registered = repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert!(repositories.share(&registered.id, true).await.is_err());
    let origin = "https://example.test/team/repo.git";
    git(&path, &["remote", "add", "origin", origin]);
    let shared = repositories.share(&registered.id, true).await.unwrap();
    assert_eq!(shared.shared_url.as_deref(), Some(origin));
    let listed = repositories.repository(&registered.id).await.unwrap();
    assert_eq!(listed.shared_url.as_deref(), Some(origin));
    let unshared = repositories.share(&registered.id, false).await.unwrap();
    assert_eq!(unshared.shared_url, None);
}
