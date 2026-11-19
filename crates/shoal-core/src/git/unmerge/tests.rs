use std::fs;

use shoal_refs::CommitId;

use crate::test_support::{commit, git, repository};

fn rev(repo: &std::path::Path, revision: &str) -> String {
    git(repo, &["rev-parse", revision]).trim().to_owned()
}

/// `main` with `first` merged as a merge commit and `stacked`, which forked
/// from `first`, merged after it.
fn stacked_history(root: &std::path::Path) -> std::path::PathBuf {
    let repo = repository(root, "repo");
    git(&repo, &["switch", "-q", "-c", "first"]);
    fs::write(repo.join("first"), "first\n").unwrap();
    commit(&repo, "first");
    git(&repo, &["switch", "-q", "-c", "stacked"]);
    fs::write(repo.join("stacked"), "stacked\n").unwrap();
    commit(&repo, "stacked");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "first"]);
    git(&repo, &["switch", "-q", "-c", "other", "main~1"]);
    fs::write(repo.join("other"), "other\n").unwrap();
    commit(&repo, "other");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "stacked"]);
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "other"]);
    repo
}

#[tokio::test]
async fn the_first_parent_chain_stops_at_its_base_or_misses_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = stacked_history(root.path());
    let chain = super::first_parent_chain(&repo, "main", "main~2")
        .await
        .unwrap();
    let expected = [rev(&repo, "main"), rev(&repo, "main~1")].map(CommitId::from);
    assert_eq!(chain, Some(expected.to_vec()));
    let empty = super::first_parent_chain(&repo, "main", "main")
        .await
        .unwrap();
    assert_eq!(empty, Some(Vec::new()));
    // A commit of a merged branch is not on the first-parent history.
    let missed = super::first_parent_chain(&repo, "main", "stacked")
        .await
        .unwrap();
    assert_eq!(missed, None);
}

#[tokio::test]
async fn a_later_merge_holds_the_commits_of_the_branch_it_stacked_on() {
    let root = tempfile::tempdir().unwrap();
    let repo = stacked_history(root.path());
    let (first, previous) = (rev(&repo, "first"), rev(&repo, "main~3"));
    assert_eq!(
        super::merged_parents(&repo, "main~1").await.unwrap(),
        [rev(&repo, "stacked")]
    );
    assert!(
        super::holds_commits_of(&repo, "stacked", &first, &previous)
            .await
            .unwrap()
    );
    assert!(
        !super::holds_commits_of(&repo, "other", &first, &previous)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_merge_commit_with_an_edit_of_its_own_is_told_from_a_clean_one() {
    let root = tempfile::tempdir().unwrap();
    let repo = stacked_history(root.path());
    assert!(!super::merge_has_own_changes(&repo, "main").await.unwrap());
    git(&repo, &["switch", "-q", "-c", "edited", "main~1"]);
    git(&repo, &["merge", "-q", "--no-ff", "--no-commit", "other"]);
    fs::write(repo.join("other"), "edited while merging\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "--no-edit"]);
    assert!(super::merge_has_own_changes(&repo, "edited").await.unwrap());
}

#[tokio::test]
async fn a_replay_stopped_while_merging_leaves_only_its_worktree_to_remove() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let start = rev(&repo, "main");
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("shared"), "side\n").unwrap();
    commit(&repo, "shared");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("merged"), "merged\n").unwrap();
    commit(&repo, "merged");
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "side"]);
    git(&repo, &["switch", "-q", "-c", "onto", &start]);
    fs::write(repo.join("shared"), "onto\n").unwrap();
    commit(&repo, "shared");
    git(&repo, &["switch", "-q", "main"]);
    let tip = rev(&repo, "main");
    let worktree = crate::git::scratch::UNMERGE
        .add(&repo, &tip.as_str().into())
        .await
        .unwrap();
    // Replaying the merge of side onto onto conflicts in the merge itself,
    // as a cancelled replay can stop.
    let replay = super::rebase_command(&worktree, "onto", "main~1")
        .output()
        .await
        .unwrap();
    assert!(!replay.status.success());
    git(&worktree, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]);

    crate::git::scratch::UNMERGE.remove(&repo).await.unwrap();
    assert!(!worktree.exists());
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    assert_eq!(rev(&repo, "main"), tip);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_checkout_moved_by_a_commit_or_a_switch_is_not_at_its_planned_tip() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let (main, tip) = ("main".into(), CommitId::from(rev(&repo, "main")));
    assert!(super::checkout_at(&repo, &main, &tip).await.unwrap());
    fs::write(repo.join("later"), "later\n").unwrap();
    commit(&repo, "later");
    assert!(!super::checkout_at(&repo, &main, &tip).await.unwrap());
    git(&repo, &["switch", "-q", "-c", "other", tip.as_str()]);
    assert!(!super::checkout_at(&repo, &main, &tip).await.unwrap());
}

/// `main` and `side`, which forked from it with commits to `feature` and
/// `notes`, after a commit of `main`'s own; their commits and the commit
/// `main` was at.
fn diverged(root: &std::path::Path) -> (std::path::PathBuf, CommitId) {
    let repo = repository(root, "repo");
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("feature"), "feature\n").unwrap();
    commit(&repo, "feature");
    fs::write(repo.join("notes"), "notes\n").unwrap();
    commit(&repo, "notes");
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    let previous = rev(&repo, "main").as_str().into();
    (repo, previous)
}

/// The commit `merge_result` finds for `side` merged by `method` onto
/// `previous`, in `main`'s history.
async fn result(
    repo: &std::path::Path,
    previous: &CommitId,
    method: crate::forge::items::action::MergeMethod,
) -> Option<String> {
    let source = rev(repo, "side").as_str().into();
    super::merge_result(repo, "main", previous, &source, method)
        .await
        .unwrap()
        .map(|commit| commit.to_string())
}

#[tokio::test]
async fn a_squash_merge_is_the_one_commit_with_the_merged_tree() {
    use crate::forge::items::action::MergeMethod;
    let root = tempfile::tempdir().unwrap();
    let (repo, previous) = diverged(root.path());
    git(&repo, &["merge", "-q", "--squash", "side"]);
    git(&repo, &["commit", "-q", "--no-edit"]);
    let squashed = rev(&repo, "main");
    fs::write(repo.join("later"), "later\n").unwrap();
    commit(&repo, "later");
    assert_eq!(
        result(&repo, &previous, MergeMethod::Squash).await,
        Some(squashed)
    );
    // Neither a merge commit nor a rebase of the branch is there.
    assert_eq!(result(&repo, &previous, MergeMethod::Merge).await, None);
    assert_eq!(result(&repo, &previous, MergeMethod::Rebase).await, None);

    // A commit with another tree is no squash of the branch.
    git(&repo, &["reset", "-q", "--hard", previous.as_str()]);
    fs::write(repo.join("feature"), "feature\n").unwrap();
    commit(&repo, "feature");
    assert_eq!(result(&repo, &previous, MergeMethod::Squash).await, None);
}

#[tokio::test]
async fn a_rebase_merge_is_the_chain_that_replays_the_branch_s_commits() {
    use crate::forge::items::action::MergeMethod;
    let root = tempfile::tempdir().unwrap();
    let (repo, previous) = diverged(root.path());
    // A change main already has is one the rebase leaves out.
    git(&repo, &["switch", "-q", "side"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    git(&repo, &["switch", "-q", "--detach", "side"]);
    git(&repo, &["rebase", "-q", "main"]);
    let rebased = rev(&repo, "HEAD");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--ff-only", &rebased]);
    fs::write(repo.join("later"), "later\n").unwrap();
    commit(&repo, "later");
    assert_eq!(
        rev(&repo, &format!("{rebased}~2")),
        previous.as_str(),
        "the rebase copied both commits of the branch's own"
    );
    assert_eq!(
        result(&repo, &previous, MergeMethod::Rebase).await,
        Some(rebased)
    );
    assert_eq!(result(&repo, &previous, MergeMethod::Squash).await, None);

    // Commits of the same changes by another author are no rebase of them.
    git(&repo, &["reset", "-q", "--hard", previous.as_str()]);
    for file in ["feature", "notes"] {
        fs::write(repo.join(file), format!("{file}\n")).unwrap();
        git(&repo, &["add", file]);
        git(
            &repo,
            &["-c", "user.name=Someone Else", "commit", "-q", "-m", file],
        );
    }
    assert_eq!(result(&repo, &previous, MergeMethod::Rebase).await, None);
}

#[tokio::test]
async fn a_rebase_merge_is_found_past_the_commits_it_dropped_as_empty() {
    use crate::forge::items::action::MergeMethod;
    let root = tempfile::tempdir().unwrap();
    let (repo, _) = diverged(root.path());
    // main squashes both of side's commits, and side goes on with a third.
    git(&repo, &["merge", "-q", "--squash", "side"]);
    git(&repo, &["commit", "-q", "--no-edit"]);
    let previous: CommitId = rev(&repo, "main").as_str().into();
    git(&repo, &["switch", "-q", "side"]);
    fs::write(repo.join("third"), "third\n").unwrap();
    commit(&repo, "third");
    git(&repo, &["switch", "-q", "--detach", "side"]);
    git(&repo, &["rebase", "-q", "main"]);
    let rebased = rev(&repo, "HEAD");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--ff-only", &rebased]);
    assert_eq!(rev(&repo, &format!("{rebased}~1")), previous.as_str());
    assert_eq!(
        result(&repo, &previous, MergeMethod::Rebase).await,
        Some(rebased)
    );
}

/// Commit `file` in `repo` with author and committer dates at `seconds`.
fn commit_at(repo: &std::path::Path, file: &str, seconds: u64) {
    fs::write(repo.join(file), format!("{file}\n")).unwrap();
    git(repo, &["add", file]);
    let date = format!("@{seconds} +0000");
    let mut command = std::process::Command::new("git");
    crate::test_support::isolate(&mut command, repo);
    let status = command
        .args(["commit", "-q", "-m", file])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn a_rebase_merge_of_a_branch_with_merges_is_found_whatever_its_dates() {
    use crate::forge::items::action::MergeMethod;
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let start = rev(&repo, "main");
    // side's commit is older than the branch's own, so Git's default order
    // lists it first, while the rebase copies the branch's first commit
    // first.
    git(&repo, &["switch", "-q", "-c", "side"]);
    commit_at(&repo, "side", 1_000_000_000);
    git(&repo, &["switch", "-q", "-c", "topic", &start]);
    commit_at(&repo, "first", 2_000_000_000);
    git(&repo, &["merge", "-q", "--no-edit", "side"]);
    commit_at(&repo, "last", 3_000_000_000);
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    let previous: CommitId = rev(&repo, "main").as_str().into();
    git(&repo, &["switch", "-q", "--detach", "topic"]);
    git(&repo, &["rebase", "-q", "main"]);
    let rebased = rev(&repo, "HEAD");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--ff-only", &rebased]);
    assert_eq!(rev(&repo, &format!("{rebased}~3")), previous.as_str());
    let source = rev(&repo, "topic").as_str().into();
    let found = super::merge_result(&repo, "main", &previous, &source, MergeMethod::Rebase);
    assert_eq!(
        found.await.unwrap().map(|commit| commit.to_string()),
        Some(rebased)
    );
}
