use std::{fs, path::Path};

use super::for_clone;
use crate::test_support::{commit, git, repository};

fn rev(repo: &Path, revision: &str) -> String {
    git(repo, &["rev-parse", revision]).trim().to_owned()
}

/// A repository whose `origin` has `pushed`, with `unpushed` a commit
/// past it that only the repository has.
fn pushed_and_not(root: &Path) -> std::path::PathBuf {
    let repo = repository(root, "repo");
    let origin = root.join("origin.git");
    git(root, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["branch", "pushed"]);
    git(&repo, &["push", "-q", "origin", "pushed"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["switch", "-q", "-c", "unpushed"]);
    fs::write(repo.join("local"), "local\n").unwrap();
    commit(&repo, "local");
    repo
}

#[tokio::test]
async fn a_pushed_branch_goes_by_its_origin_branch_and_an_unpushed_one_by_none() {
    let temp = tempfile::tempdir().unwrap();
    let repo = pushed_and_not(temp.path());
    let pushed = for_clone(&repo, "pushed").await.unwrap();
    assert_eq!(pushed.as_deref(), Some("origin/pushed"));
    assert_eq!(for_clone(&repo, "unpushed").await.unwrap(), None);
    // The pushed commit, named as a commit, goes by its ID.
    let commit = for_clone(&repo, "unpushed~1").await.unwrap();
    assert_eq!(commit, Some(rev(&repo, "unpushed~1")));
}
