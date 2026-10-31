use super::*;

async fn git(repo: &Path, args: &[&str]) -> String {
    crate::git::run(repo, args).await.unwrap()
}

#[tokio::test]
async fn a_detached_checkout_holds_the_commit_and_leaves_no_worktree_behind() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let commit = git(&repo, &["rev-parse", "HEAD"]).await;
    let commit = commit.trim();
    std::fs::write(repo.join("uncommitted"), "edit").unwrap();
    let checkout = root.path().join("reviews").join("1");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    add(&repo, &checkout, commit).await.unwrap();
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]).await.trim(), commit);
    assert!(
        git(&checkout, &["branch", "--show-current"])
            .await
            .trim()
            .is_empty()
    );
    assert!(!checkout.join("uncommitted").exists());
    std::fs::write(checkout.join("scratch"), "left by the reviewer").unwrap();
    remove(&checkout).await.unwrap();
    assert!(!checkout.exists());
    let listed = git(&repo, &["worktree", "list", "--porcelain"]).await;
    assert!(!listed.contains("reviews/1"), "{listed}");
    remove(&checkout).await.unwrap();
}
