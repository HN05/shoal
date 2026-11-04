use std::fs;

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
    assert_eq!(chain, Some(vec![rev(&repo, "main"), rev(&repo, "main~1")]));
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
async fn a_replay_stopped_while_merging_is_aborted() {
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
    // Replaying the merge of side onto onto conflicts in the merge itself.
    let replay = super::rebase_command(&repo, "onto", "main~1")
        .output()
        .await
        .unwrap();
    assert!(!replay.status.success());
    git(&repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]);

    super::abort_replay(&repo).await.unwrap();
    assert_eq!(rev(&repo, "main"), tip);
    assert!(!repo.join(".git/rebase-merge").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}
