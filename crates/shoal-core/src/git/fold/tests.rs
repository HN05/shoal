use std::{
    fs,
    path::{Path, PathBuf},
};

use shoal_refs::{BranchName, CommitId};

use super::{Fold, fold};
use crate::{
    forge::items::action::MergeMethod,
    git::merge_into::{Source, Target},
    test_support::{commit, git, repository},
};

fn rev(repo: &Path, revision: &str) -> CommitId {
    git(repo, &["rev-parse", revision]).trim().into()
}

/// `main` checked out in the returned repository, with `kept` and `edited`,
/// and a worktree of `side` beside it with a commit adding `feature`.
fn repo_and_side(root: &Path) -> (PathBuf, PathBuf) {
    let repo = repository(root, "repo");
    for file in ["kept", "edited", "deleted"] {
        fs::write(repo.join(file), format!("{file}\n")).unwrap();
        commit(&repo, file);
    }
    let side = root.join("side");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            side.to_str().unwrap(),
        ],
    );
    fs::write(side.join("feature"), "feature\n").unwrap();
    commit(&side, "feature");
    (repo, side)
}

/// Fold `side`'s branch, by a merge, and its unsaved files into `main`.
async fn fold_side(repo: &Path, side: &Path) -> anyhow::Result<super::Folded> {
    let (side_branch, main) = (BranchName::from("side"), BranchName::from("main"));
    let (source, head) = (rev(side, "HEAD"), rev(repo, "HEAD"));
    fold(Fold {
        repo,
        source: Some(Source {
            branch: &side_branch,
            commit: &source,
            method: MergeMethod::Merge,
            message: Some("Absorb side"),
        }),
        carry_from: Some(side),
        into: Target {
            branch: &main,
            commit: &head,
            worktree: Some(repo),
        },
    })
    .await
}

#[tokio::test]
async fn a_fold_merges_the_branch_and_carries_its_unsaved_files_as_unsaved_changes() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::write(side.join("edited"), "side edit\n").unwrap();
    fs::remove_file(side.join("deleted")).unwrap();
    fs::write(side.join("untracked"), "new\n").unwrap();
    fs::write(repo.join("kept"), "main's own edit\n").unwrap();
    let side_status = git(&side, &["status", "--porcelain"]);

    let folded = fold_side(&repo, &side).await.unwrap();

    assert!(folded.merged.unwrap().updated);
    assert_eq!(folded.commits.len(), 1);
    assert_eq!(folded.carried, ["deleted", "edited", "untracked"]);
    assert_eq!(rev(&repo, "main"), rev(&side, "HEAD"));
    assert_eq!(
        git(&repo, &["status", "--porcelain"]),
        " D deleted\n M edited\n M kept\n?? untracked\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("edited")).unwrap(),
        "side edit\n"
    );
    assert_eq!(git(&side, &["status", "--porcelain"]), side_status);
}

#[tokio::test]
async fn a_fold_that_touches_the_receiving_worktrees_unsaved_files_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::write(repo.join("feature"), "main's untracked feature\n").unwrap();
    let before = (rev(&repo, "main"), git(&repo, &["status", "--porcelain"]));

    let merging = fold_side(&repo, &side).await.unwrap_err();
    assert!(format!("{merging:#}").contains("feature"), "{merging:#}");
    assert_eq!(
        (rev(&repo, "main"), git(&repo, &["status", "--porcelain"])),
        before
    );

    fs::remove_file(repo.join("feature")).unwrap();
    fs::write(repo.join("edited"), "main's edit\n").unwrap();
    fs::write(side.join("edited"), "side's edit\n").unwrap();
    let before = (rev(&repo, "main"), git(&repo, &["status", "--porcelain"]));
    let carrying = fold_side(&repo, &side).await.unwrap_err();
    assert!(format!("{carrying:#}").contains("edited"), "{carrying:#}");
    assert_eq!(
        (rev(&repo, "main"), git(&repo, &["status", "--porcelain"])),
        before
    );
}

#[tokio::test]
async fn a_fold_never_replaces_a_file_the_receiving_worktree_ignores() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::write(repo.join(".git/info/exclude"), ".env\n").unwrap();
    fs::write(repo.join(".env"), "the receiver's secret\n").unwrap();
    fs::write(side.join(".env"), "the source's\n").unwrap();
    git(&side, &["add", "--force", ".env"]);

    let refused = fold_side(&repo, &side).await.unwrap_err();

    assert!(format!("{refused:#}").contains(".env"), "{refused:#}");
    let kept = fs::read_to_string(repo.join(".env")).unwrap();
    assert_eq!(kept, "the receiver's secret\n");
    assert_ne!(rev(&repo, "main"), rev(&side, "HEAD"));
}

#[tokio::test]
async fn a_carried_edit_follows_a_rename_the_receiving_branch_made() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    git(&repo, &["mv", "edited", "renamed"]);
    git(&repo, &["commit", "-q", "-m", "Rename edited"]);
    fs::write(side.join("edited"), "edited\nside edit\n").unwrap();

    let folded = fold_side(&repo, &side).await.unwrap();

    assert_eq!(folded.carried, ["renamed"]);
    assert_eq!(git(&repo, &["status", "--porcelain"]), " M renamed\n");
    let renamed = fs::read_to_string(repo.join("renamed")).unwrap();
    assert_eq!(renamed, "edited\nside edit\n");
}

#[tokio::test]
async fn a_carried_file_never_replaces_a_directory_of_the_receivers_untracked_files() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::create_dir(repo.join("notes")).unwrap();
    fs::write(repo.join("notes/local.txt"), "the receiver's\n").unwrap();
    fs::write(side.join("notes"), "the source's\n").unwrap();

    let refused = fold_side(&repo, &side).await.unwrap_err();

    assert!(format!("{refused:#}").contains("notes"), "{refused:#}");
    let kept = fs::read_to_string(repo.join("notes/local.txt")).unwrap();
    assert_eq!(kept, "the receiver's\n");
}

#[tokio::test]
async fn unsaved_edits_the_receiver_already_committed_carry_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::write(repo.join("edited"), "both\n").unwrap();
    commit(&repo, "edited");
    fs::write(side.join("edited"), "both\n").unwrap();

    let folded = fold_side(&repo, &side).await.unwrap();

    assert!(folded.carried.is_empty());
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_carried_file_never_replaces_one_a_case_insensitive_receiver_names_otherwise() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    git(&repo, &["config", "core.ignorecase", "true"]);
    fs::write(repo.join("Notes"), "the receiver's\n").unwrap();
    fs::write(side.join("notes"), "the source's\n").unwrap();

    let refused = fold_side(&repo, &side).await.unwrap_err();

    assert!(format!("{refused:#}").contains("notes"), "{refused:#}");
    let kept = fs::read_to_string(repo.join("Notes")).unwrap();
    assert_eq!(kept, "the receiver's\n");
}

#[tokio::test]
async fn folding_again_carries_nothing_the_receiver_already_took() {
    let root = tempfile::tempdir().unwrap();
    let (repo, side) = repo_and_side(root.path());
    fs::write(side.join("edited"), "side edit\n").unwrap();
    fs::write(side.join("untracked"), "new\n").unwrap();
    fold_side(&repo, &side).await.unwrap();
    let carried = git(&repo, &["status", "--porcelain"]);

    let again = fold_side(&repo, &side).await.unwrap();

    assert!(again.carried.is_empty(), "{:?}", again.carried);
    assert_eq!(git(&repo, &["status", "--porcelain"]), carried);
}
