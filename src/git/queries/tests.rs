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
