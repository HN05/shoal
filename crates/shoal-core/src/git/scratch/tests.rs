use std::fs;

use shoal_refs::CommitId;

use super::UNMERGE;
use crate::test_support::{git, repository};

fn rev(repo: &std::path::Path, revision: &str) -> String {
    git(repo, &["rev-parse", revision]).trim().to_owned()
}

#[tokio::test]
async fn a_scratch_directory_shoal_did_not_create_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let directory = UNMERGE.directory(&repo).await.unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("notes"), "someone else's\n").unwrap();
    for error in [
        UNMERGE.remove(&repo).await.unwrap_err(),
        UNMERGE.add(&repo, &"main".into()).await.unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("Shoal did not create it"),
            "{error:#}"
        );
    }
    assert_eq!(
        fs::read_to_string(directory.join("notes")).unwrap(),
        "someone else's\n"
    );
}

#[tokio::test]
async fn removing_a_scratch_worktree_keeps_another_worktree_s_admin_directory() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let workspace = root.path().join("workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            workspace.to_str().unwrap(),
        ],
    );
    let stopped = crate::git::worktree::own_admin_dir(&workspace)
        .await
        .unwrap()
        .unwrap();
    // A workspace removal stopped after Git deleted the `.git` link.
    fs::remove_file(workspace.join(".git")).unwrap();
    let worktree = UNMERGE
        .add(&repo, &rev(&repo, "main").as_str().into())
        .await
        .unwrap();

    UNMERGE.remove(&repo).await.unwrap();
    assert!(!worktree.exists());
    assert!(stopped.exists());
}

#[tokio::test]
async fn a_scratch_worktree_replaces_the_registration_of_one_whose_directory_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let tip: CommitId = rev(&repo, "main").as_str().into();
    let worktree = UNMERGE.add(&repo, &tip).await.unwrap();
    fs::remove_dir_all(&worktree).unwrap();

    let replaced = UNMERGE.add(&repo, &tip).await.unwrap();
    assert_eq!(git(&replaced, &["rev-parse", "HEAD"]).trim(), tip.as_str());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
}
