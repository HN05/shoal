use std::fs;

use crate::test_support::{commit, git, repository};

#[tokio::test]
async fn a_conflicted_merge_lists_its_conflicted_paths() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::write(repo.join("untouched"), "untouched\n").unwrap();
    commit(&repo, "untouched");
    git(&repo, &["switch", "-q", "-c", "other"]);
    fs::write(repo.join("tracked"), "other\n").unwrap();
    commit(&repo, "tracked");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("tracked"), "main\n").unwrap();
    commit(&repo, "tracked");
    let paths = super::merge_conflicts(&repo, "main", "other")
        .await
        .unwrap();
    assert_eq!(paths, ["tracked"]);
}
