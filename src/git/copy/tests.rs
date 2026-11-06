use super::*;

async fn git(repo: &Path, args: &[&str]) -> String {
    crate::git::run(repo, args).await.unwrap()
}

/// A worktree with a staged, an unstaged, an untracked, a deleted and an
/// ignored file, and HEAD's commit.
async fn changed_worktree(repo: &Path) -> String {
    std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
    std::fs::write(repo.join("deleted"), "deleted\n").unwrap();
    std::fs::write(repo.join("unstaged"), "unstaged\n").unwrap();
    git(repo, &["add", "."]).await;
    git(repo, &["commit", "-m", "files"]).await;
    std::fs::write(repo.join("tracked"), "staged\n").unwrap();
    git(repo, &["add", "tracked"]).await;
    std::fs::write(repo.join("tracked"), "staged then changed\n").unwrap();
    std::fs::write(repo.join("unstaged"), "changed\n").unwrap();
    std::fs::remove_file(repo.join("deleted")).unwrap();
    std::fs::create_dir_all(repo.join("nested/dir")).unwrap();
    std::fs::write(repo.join("nested/dir/untracked"), "untracked\n").unwrap();
    std::fs::create_dir_all(repo.join("build")).unwrap();
    std::fs::write(repo.join("build/output"), "ignored\n").unwrap();
    git(repo, &["rev-parse", "HEAD"]).await.trim().to_owned()
}

#[tokio::test]
async fn a_copy_has_the_source_index_and_files_on_a_new_branch() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let head = changed_worktree(&repo).await;
    let copy = root.path().join("copy");
    add_worktree(&repo, &copy, Some("repo-copy"), &head)
        .await
        .unwrap();
    fill(&repo, &copy, &head).await.unwrap();
    assert_eq!(
        git(&copy, &["branch", "--show-current"]).await.trim(),
        "repo-copy"
    );
    for args in [
        &["status", "--porcelain", "--untracked-files=all"][..],
        &["diff", "--cached"],
        &["diff"],
    ] {
        assert_eq!(git(&copy, args).await, git(&repo, args).await, "{args:?}");
    }
    assert_eq!(
        std::fs::read_to_string(copy.join("build/output")).unwrap(),
        "ignored\n"
    );
    assert!(!copy.join("deleted").exists());
    // The source keeps its own worktree.
    assert_eq!(
        git(&repo, &["branch", "--show-current"]).await.trim(),
        "main"
    );
}

#[tokio::test]
async fn a_detached_copy_has_no_branch() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let head = changed_worktree(&repo).await;
    let copy = root.path().join("copy");
    add_worktree(&repo, &copy, None, &head).await.unwrap();
    fill(&repo, &copy, &head).await.unwrap();
    assert!(
        git(&copy, &["branch", "--show-current"])
            .await
            .trim()
            .is_empty()
    );
    assert_eq!(git(&copy, &["rev-parse", "HEAD"]).await.trim(), head);
    assert_eq!(
        git(&copy, &["status", "--porcelain"]).await,
        git(&repo, &["status", "--porcelain"]).await
    );
}

#[tokio::test]
async fn a_source_whose_head_moved_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let head = changed_worktree(&repo).await;
    let copy = root.path().join("copy");
    add_worktree(&repo, &copy, Some("repo-copy"), &head)
        .await
        .unwrap();
    git(&repo, &["commit", "-m", "moved"]).await;
    let error = fill(&repo, &copy, &head).await.unwrap_err();
    assert!(format!("{error:#}").contains("moved"), "{error:#}");
}

#[tokio::test]
async fn checked_out_submodules_and_ignored_linked_worktrees_are_shared() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repository(root.path(), "repo");
    let library = crate::test_support::repository(root.path(), "library");
    let library = library.to_str().unwrap();
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            library,
            "lib",
        ],
    )
    .await;
    git(&repo, &["commit", "-m", "submodule"]).await;
    assert_eq!(shared_metadata(&repo).await.unwrap(), ["lib"]);
    git(&repo, &["submodule", "deinit", "--force", "lib"]).await;
    assert!(shared_metadata(&repo).await.unwrap().is_empty());
    // A linked worktree inside an ignored directory names the repository's
    // metadata; an ignored clone has its own.
    std::fs::write(repo.join(".gitignore"), "worktrees/\nclones/\n").unwrap();
    std::fs::create_dir_all(repo.join("worktrees")).unwrap();
    let worktree = repo.join("worktrees/nested");
    git(
        &repo,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()],
    )
    .await;
    git(&repo, &["clone", "--quiet", library, "clones/library"]).await;
    assert_eq!(shared_metadata(&repo).await.unwrap(), ["worktrees/nested"]);
}
