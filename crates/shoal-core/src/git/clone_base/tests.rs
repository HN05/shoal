use std::{fs, path::Path};

use super::{bundle_carried, carried_commit, fetch_carried, for_clone};
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
    let pushed = for_clone(&repo, "pushed").await.unwrap().unwrap();
    assert_eq!(
        (pushed.base.as_str(), pushed.branch.as_str()),
        ("origin/pushed", "pushed")
    );
    assert_eq!(for_clone(&repo, "unpushed").await.unwrap(), None);
    // The pushed commit, named as a commit, goes by its ID, which the
    // origin's branch holds.
    let commit = for_clone(&repo, "unpushed~1").await.unwrap().unwrap();
    assert_eq!(commit.base, rev(&repo, "unpushed~1"));
    assert_eq!(commit.branch, "pushed");
}

#[tokio::test]
async fn a_clone_of_the_origin_fetches_an_unpushed_commit_from_its_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let repo = pushed_and_not(temp.path());
    let commit = carried_commit(&repo, "unpushed").await.unwrap();
    let file = temp.path().join("carried.bundle");
    bundle_carried(&repo, &file, (&commit, "pushed"))
        .await
        .unwrap();
    let origin = temp.path().join("origin.git");
    let clone = temp.path().join("clone");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    fetch_carried(&clone, &file, (&commit, "pushed"))
        .await
        .unwrap();
    let fetched = rev(&clone, &format!("refs/shoal/carried/{commit}"));
    assert_eq!(fetched, commit.as_str());
    // The repository that bundled it keeps no ref of it.
    let refs = git(&repo, &["for-each-ref", "refs/shoal"]);
    assert!(refs.trim().is_empty(), "{refs}");
}

#[tokio::test]
async fn bundles_of_one_commit_at_once_each_keep_their_own_ref() {
    let temp = tempfile::tempdir().unwrap();
    let repo = pushed_and_not(temp.path());
    let commit = carried_commit(&repo, "unpushed").await.unwrap();
    let (first, second) = (temp.path().join("first"), temp.path().join("second"));
    let carried = (&commit, "pushed");
    let (first, second) = tokio::join!(
        bundle_carried(&repo, &first, carried),
        bundle_carried(&repo, &second, carried),
    );
    first.unwrap();
    second.unwrap();
}
