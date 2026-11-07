use super::*;
use crate::test_support::{git, repository};

#[tokio::test]
async fn push_remote_prefers_the_branch_then_the_default_then_origin() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    assert_eq!(push_remote(&repo, "topic").await.unwrap(), "origin");
    git(&repo, &["config", "remote.pushDefault", "fork"]);
    assert_eq!(push_remote(&repo, "topic").await.unwrap(), "fork");
    git(&repo, &["config", "branch.topic.pushRemote", "mine"]);
    assert_eq!(push_remote(&repo, "topic").await.unwrap(), "mine");
    assert_eq!(push_remote(&repo, "other").await.unwrap(), "fork");
}

#[tokio::test]
async fn head_and_branch_names_no_branch_when_detached() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["checkout", "-q", "-b", "feature/x"]);
    let commit = revision(&repo, "HEAD", run)
        .await
        .unwrap()
        .trim()
        .to_owned();
    let on_branch = head_and_branch(&repo).await.unwrap();
    assert_eq!(on_branch, (commit.clone(), Some("feature/x".to_owned())));
    git(&repo, &["checkout", "-q", "--detach"]);
    assert_eq!(head_and_branch(&repo).await.unwrap(), (commit, None));
}

#[test]
fn head_and_branch_without_a_name_for_head_is_unknown() {
    assert!(parse_head_and_branch("abc\n").is_err());
    assert_eq!(
        parse_head_and_branch("abc\nHEAD\n").unwrap(),
        ("abc".to_owned(), None)
    );
}

#[tokio::test]
async fn checkout_dirs_tell_a_linked_worktree_from_its_main_checkout() {
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
            "topic",
            linked.to_str().unwrap(),
        ],
    );
    std::fs::create_dir(linked.join("sub")).unwrap();
    let main = checkout_dirs(&repo).await.unwrap();
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(
        main,
        CheckoutDirs {
            root: std::fs::canonicalize(&repo).unwrap(),
            common_dir: common.clone(),
            git_dir: common.clone(),
        }
    );
    let worktree = checkout_dirs(&linked.join("sub")).await.unwrap();
    assert_eq!(
        worktree,
        CheckoutDirs {
            root: std::fs::canonicalize(&linked).unwrap(),
            common_dir: common.clone(),
            git_dir: common.join("worktrees/linked"),
        }
    );
}
