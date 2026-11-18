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

#[tokio::test]
async fn a_checkout_killed_while_it_was_created_is_replaced() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let commit = git(&repo, &["rev-parse", "HEAD"]).await;
    let checkout = root.path().join("reviews").join("1");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    add(&repo, &checkout, commit.trim()).await.unwrap();
    // What a killed `git worktree add` leaves: a locked registration whose
    // directory is gone.
    git(&repo, &["worktree", "lock", checkout.to_str().unwrap()]).await;
    remove_writable(&checkout);
    add(&repo, &checkout, commit.trim()).await.unwrap();
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]).await.trim(),
        commit.trim()
    );
    remove(&checkout).await.unwrap();
}

/// Delete the read-only checkout's directory, leaving its registration.
fn remove_writable(checkout: &Path) {
    set_writable(checkout, true).unwrap();
    std::fs::remove_dir_all(checkout).unwrap();
}

/// What a workspace removal stopped after Git deleted the `.git` link
/// leaves: the worktree's directory and its admin directory, which the next
/// removal reads.
async fn stopped_removal(repo: &Path, path: &Path) -> std::path::PathBuf {
    let commit = git(repo, &["rev-parse", "HEAD"]).await;
    add_workspace(repo, path, commit.trim()).await.unwrap();
    let admin_dir = crate::git::worktree::own_admin_dir(path)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(path.join(".git")).unwrap();
    admin_dir
}

#[tokio::test]
async fn a_checkout_git_cannot_remove_takes_only_its_own_registration() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let commit = git(&repo, &["rev-parse", "HEAD"]).await;
    let stopped = stopped_removal(&repo, &root.path().join("workspace")).await;
    let checkout = root.path().join("reviews").join("1");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    add(&repo, &checkout, commit.trim()).await.unwrap();
    let own = crate::git::worktree::own_admin_dir(&checkout)
        .await
        .unwrap()
        .unwrap();
    // A locked checkout, which `worktree remove --force` refuses.
    git(&repo, &["worktree", "lock", checkout.to_str().unwrap()]).await;

    remove(&checkout).await.unwrap();
    assert!(!checkout.exists());
    assert!(!own.exists());
    assert!(stopped.exists());
}

#[tokio::test]
async fn a_checkout_with_a_relative_link_git_cannot_remove_takes_its_registration() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    git(&repo, &["config", "worktree.useRelativePaths", "true"]).await;
    let commit = git(&repo, &["rev-parse", "HEAD"]).await;
    let checkout = root.path().join("reviews").join("1");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    add(&repo, &checkout, commit.trim()).await.unwrap();
    let link = std::fs::read_to_string(checkout.join(".git")).unwrap();
    assert!(link.starts_with("gitdir: .."), "{link}");
    git(&repo, &["worktree", "lock", checkout.to_str().unwrap()]).await;

    remove(&checkout).await.unwrap();
    assert!(!checkout.exists());
    let listed = git(&repo, &["worktree", "list", "--porcelain"]).await;
    assert!(!listed.contains("reviews/1"), "{listed}");
}
