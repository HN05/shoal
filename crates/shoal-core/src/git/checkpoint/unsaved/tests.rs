use std::{fs, path::Path};

use super::*;
use crate::test_support::{git, repository};

/// The size `git diff HEAD` and `git ls-files --others` list for `repo`'s
/// unsaved files, which [`unsaved_bytes`] must count.
fn listed_size(repo: &Path) -> u64 {
    let changed = git(repo, &["diff", "-z", "--name-only", "HEAD", "--"]);
    let untracked = git(repo, &["ls-files", "-z", "--others", "--exclude-standard"]);
    (changed.split('\0').chain(untracked.split('\0')))
        .filter(|file| !file.is_empty())
        .filter_map(|file| fs::symlink_metadata(repo.join(file)).ok())
        .map(|metadata| metadata.len())
        .sum()
}

async fn assert_counts_as_listed(repo: &Path) {
    let status = git(repo, &["status", "--porcelain"]);
    assert_eq!(
        unsaved_bytes(repo).await.unwrap(),
        Some(listed_size(repo)),
        "{status}"
    );
}

#[tokio::test]
async fn a_staged_change_the_worktree_undoes_counts_as_a_diff_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::write(repo.join("tracked"), "staged\n").unwrap();
    git(&repo, &["add", "tracked"]);
    fs::write(repo.join("tracked"), "initial\n").unwrap();
    fs::write(repo.join("untracked"), "new\n").unwrap();
    assert_eq!(unsaved_bytes(&repo).await.unwrap(), Some(4));
    assert_counts_as_listed(&repo).await;
}

#[tokio::test]
async fn a_file_removed_from_the_index_counts_as_a_diff_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["rm", "-q", "--cached", "tracked"]);
    assert_counts_as_listed(&repo).await;
}

#[tokio::test]
async fn a_staged_rename_counts_as_a_diff_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["mv", "tracked", "renamed"]);
    assert_counts_as_listed(&repo).await;
    fs::write(repo.join("tracked"), "initial\n").unwrap();
    assert_counts_as_listed(&repo).await;
}

#[tokio::test]
async fn an_intent_to_add_file_counts_as_a_diff_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::write(repo.join("intended"), "later\n").unwrap();
    git(&repo, &["add", "--intent-to-add", "intended"]);
    assert_counts_as_listed(&repo).await;
}

#[tokio::test]
async fn a_conflicted_file_counts_as_a_diff_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["checkout", "-q", "-b", "other"]);
    fs::write(repo.join("tracked"), "other\n").unwrap();
    crate::test_support::commit(&repo, "tracked");
    git(&repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("tracked"), "main\n").unwrap();
    crate::test_support::commit(&repo, "tracked");
    let program = crate::test_support::developer_tools()
        .map_or_else(|| "git".into(), |tools| tools.join("git"));
    let merge = crate::test_support::isolate(&mut std::process::Command::new(program), &repo)
        .args(["merge", "-q", "other"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge conflicts");
    assert_counts_as_listed(&repo).await;
}

#[test]
fn a_changed_entry_is_counted_as_its_code_tells() {
    let entry = |code: &str, path: &str| {
        format!(
            "1 {code} N... 100644 100644 100644 {0} {0} {path}\0",
            "0".repeat(40)
        )
    };
    let status = [
        entry(".M", "worktree only"),
        entry("A.", "added"),
        entry("MM", "both"),
        entry("D.", "removed from the index"),
        "? untracked file\0".to_owned(),
    ]
    .concat();
    let unsaved = parse_status(&status).unwrap();
    assert_eq!(unsaved.changed, ["worktree only", "added"]);
    assert_eq!(unsaved.undecided, ["both", "removed from the index"]);
    assert_eq!(unsaved.untracked, ["untracked file"]);
    assert_eq!(parse_status("? nested/\0"), None);
}
