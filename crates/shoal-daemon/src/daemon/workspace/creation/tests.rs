use super::*;
use std::fs;

#[tokio::test]
async fn existing_base_falls_back_only_when_the_default_ref_is_missing() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    // The cached remote HEAD names the default branch without reading it.
    crate::daemon::test_support::git(&path, &["switch", "-c", "topic"]);
    crate::daemon::test_support::git(&path, &["remote", "add", "origin", "/missing"]);
    crate::daemon::test_support::git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let topic = crate::daemon::test_support::git(&path, &["rev-parse", "topic"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert_eq!(
        existing_base(&repo, "topic").await.unwrap(),
        "refs/heads/main"
    );
    let main = path.join(".git/refs/heads/main");
    fs::write(&main, "garbage\n").unwrap();
    assert!(existing_base(&repo, "topic").await.is_err());
    fs::remove_file(&main).unwrap();
    assert_eq!(existing_base(&repo, "topic").await.unwrap(), topic.trim());
    manager.store.shutdown().await;
}
