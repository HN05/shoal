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
    // Root writes whatever the permissions say.
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert!(std::fs::write(checkout.join("scratch"), "left by the reviewer").is_err());
        let tracked = std::fs::read_dir(&checkout)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_file() && !path.ends_with(".git"))
            .unwrap();
        assert!(std::fs::write(&tracked, "changed").is_err());
    }
    remove(&checkout).await.unwrap();
    assert!(!checkout.exists());
    let listed = git(&repo, &["worktree", "list", "--porcelain"]).await;
    assert!(!listed.contains("reviews/1"), "{listed}");
    remove(&checkout).await.unwrap();
}

