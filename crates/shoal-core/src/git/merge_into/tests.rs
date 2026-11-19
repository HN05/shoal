use std::{fs, path::Path};

use shoal_refs::{BranchName, CommitId};

use super::{Source, Target, merge};
use crate::{
    forge::items::action::MergeMethod,
    test_support::{commit, git, repository},
};

fn rev(repo: &Path, revision: &str) -> CommitId {
    git(repo, &["rev-parse", revision]).trim().into()
}

/// `main`, checked out in `repo`, with a commit to `main` after `side`
/// forked from it with a commit to `feature`.
fn diverged(root: &Path) -> std::path::PathBuf {
    let repo = repository(root, "repo");
    fs::write(repo.join("shared"), "shared\n").unwrap();
    commit(&repo, "shared");
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("feature"), "feature\n").unwrap();
    commit(&repo, "feature");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    repo
}

/// Merge `side` into `main`, checked out in `repo`, by `method`.
async fn merge_side(repo: &Path, method: MergeMethod) -> anyhow::Result<super::Merged> {
    let (side, main) = (BranchName::from("side"), BranchName::from("main"));
    let (source, previous) = (rev(repo, "side"), rev(repo, "main"));
    let source = Source {
        branch: &side,
        commit: &source,
        method,
        message: None,
    };
    let target = Target {
        branch: &main,
        commit: &previous,
        worktree: Some(repo),
    };
    merge(repo, &source, &target).await
}

#[tokio::test]
async fn a_merge_commit_goes_into_a_worktree_keeping_its_staged_and_unstaged_changes() {
    let root = tempfile::tempdir().unwrap();
    let repo = diverged(root.path());
    fs::write(repo.join("shared"), "staged\n").unwrap();
    git(&repo, &["add", "shared"]);
    fs::write(repo.join("main"), "unstaged\n").unwrap();
    let previous = rev(&repo, "main");

    let merged = merge_side(&repo, MergeMethod::Merge).await.unwrap();

    assert!(merged.updated && !merged.fast_forward);
    assert_eq!(merged.commit, rev(&repo, "HEAD"));
    let parents = git(&repo, &["show", "-s", "--format=%P", "HEAD"]);
    assert_eq!(parents.trim(), format!("{previous} {}", rev(&repo, "side")));
    assert_eq!(
        git(&repo, &["show", "-s", "--format=%s", "HEAD"]).trim(),
        "Merge branch 'side'"
    );
    assert_eq!(
        fs::read_to_string(repo.join("feature")).unwrap(),
        "feature\n"
    );
    assert_eq!(
        git(&repo, &["status", "--porcelain"]),
        " M main\nM  shared\n"
    );
}

#[tokio::test]
async fn a_branch_the_target_forked_from_fast_forwards_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("feature"), "feature\n").unwrap();
    commit(&repo, "feature");
    git(&repo, &["switch", "-q", "main"]);

    let merged = merge_side(&repo, MergeMethod::Merge).await.unwrap();

    assert!(merged.updated && merged.fast_forward);
    assert_eq!(rev(&repo, "main"), rev(&repo, "side"));
}

#[tokio::test]
async fn a_merge_that_conflicts_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = diverged(root.path());
    git(&repo, &["switch", "-q", "side"]);
    fs::write(repo.join("main"), "side\n").unwrap();
    commit(&repo, "main");
    git(&repo, &["switch", "-q", "main"]);
    let before = rev(&repo, "main");

    for method in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
        let error = merge_side(&repo, method).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("does not apply cleanly"),
            "{method}: {error:#}"
        );
        assert_eq!(rev(&repo, "main"), before, "{method}");
        assert_eq!(git(&repo, &["status", "--porcelain"]), "", "{method}");
    }
}
