use super::*;
use crate::test_support;

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
