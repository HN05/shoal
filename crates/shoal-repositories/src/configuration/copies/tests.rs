use shoal_core::test_support::repository;

use crate::stand_in::Fixture;

#[tokio::test]
async fn a_copy_replaces_the_local_config_only_while_it_reads_what_was_compared() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let checkout = repository(fixture.root.path(), "repo");
    let repo = repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let path = repositories.local_config_path(&repo.id).await.unwrap();
    let take = |expected: Option<&str>, text: Option<&str>, keep| {
        let (expected, text) = (expected.map(str::to_owned), text.map(str::to_owned));
        repositories.take_local_config_copy(&fixture.daemon, &repo.id, expected, text, keep)
    };
    assert_eq!(take(None, Some("first\n"), true).await.unwrap(), None);
    let read = || repositories.local_config_text(&repo.id);
    assert_eq!(read().await.unwrap().as_deref(), Some("first\n"));
    assert!(take(None, Some("second\n"), true).await.is_err());
    assert_eq!(read().await.unwrap().as_deref(), Some("first\n"));
    let kept = take(Some("first\n"), None, true).await.unwrap();
    assert_eq!(kept, Some(path.with_file_name("config.toml.backup")));
    assert_eq!(read().await.unwrap(), None);
    let backup = std::fs::read_to_string(kept.unwrap()).unwrap();
    assert_eq!(backup, "first\n");
}
