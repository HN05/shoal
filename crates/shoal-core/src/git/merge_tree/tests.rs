use std::fs;

use super::*;
use crate::test_support::{self, commit, git};

#[tokio::test]
async fn a_failure_on_a_current_git_reports_only_gits_error() {
    let root = tempfile::tempdir().unwrap();
    let repo = test_support::repository(root.path(), "repo");
    std::fs::write(repo.join(".git/config"), "[broken\n").unwrap();
    let error = merged_tree(&repo, "HEAD", "HEAD").await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("bad config"), "{error}");
    assert!(!error.contains("or newer is required"), "{error}");
}

#[tokio::test]
async fn a_conflicted_merge_has_no_tree() {
    let root = tempfile::tempdir().unwrap();
    let repo = test_support::repository(root.path(), "repo");
    git(&repo, &["switch", "-q", "-c", "other"]);
    fs::write(repo.join("tracked"), "other\n").unwrap();
    commit(&repo, "tracked");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("tracked"), "main\n").unwrap();
    commit(&repo, "tracked");
    assert_eq!(merged_tree(&repo, "main", "other").await.unwrap(), None);
}

#[tokio::test]
async fn a_revision_git_cannot_merge_is_gits_error_not_a_conflict() {
    let root = tempfile::tempdir().unwrap();
    let repo = test_support::repository(root.path(), "repo");
    let error = merged_tree(&repo, "HEAD", "missing").await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("not something we can merge"), "{error}");
}
