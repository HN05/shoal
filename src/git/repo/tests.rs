use std::fs;

use crate::test_support::{git, repository};

#[tokio::test]
async fn unfinished_operation_reads_the_checkouts_own_git_dir() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    assert_eq!(super::unfinished_operation(&repo).await.unwrap(), None);

    let linked_git_dir = repo.join(".git/worktrees/linked");
    fs::create_dir(linked_git_dir.join("rebase-merge")).unwrap();
    assert_eq!(
        super::unfinished_operation(&linked).await.unwrap(),
        Some("rebase-merge")
    );
    assert_eq!(super::unfinished_operation(&repo).await.unwrap(), None);

    fs::write(repo.join(".git/MERGE_HEAD"), "").unwrap();
    assert_eq!(
        super::unfinished_operation(&repo).await.unwrap(),
        Some("MERGE_HEAD")
    );
}
