use std::path::{Path, PathBuf};

use anyhow::bail;

use super::*;
use crate::test_support::{git, repository};

/// The worktrees of `repo` as gix reads them; `None` where it leaves the
/// listing to Git.
async fn read_in_process(repo: &Path) -> Option<Vec<Worktree>> {
    reader::read(
        repo,
        |repo| Ok(in_process(repo)),
        async |_| bail!("no repository"),
    )
    .await
    .unwrap()
}

/// The worktrees of `repo` as Git lists them.
fn listed_by_git(repo: &Path) -> Vec<Worktree> {
    parse_worktrees(&git(repo, &["worktree", "list", "--porcelain", "-z"]))
}

/// Assert that gix reads the worktrees of the repository at each of
/// `paths` as Git lists them.
async fn assert_read_as_git_lists(paths: &[PathBuf]) {
    for path in paths {
        let read = read_in_process(path).await;
        assert_eq!(read, Some(listed_by_git(path)), "{}", path.display());
    }
}

#[tokio::test]
async fn reads_main_linked_detached_locked_and_prunable_worktrees_as_git_lists_them() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["worktree", "add", "-b", "topic", "../topic"]);
    git(&repo, &["worktree", "add", "--detach", "../detached"]);
    git(&repo, &["worktree", "add", "-b", "held", "../held"]);
    git(
        &repo,
        &["worktree", "lock", "--reason", "on a disk", "../held"],
    );
    git(&repo, &["worktree", "add", "-b", "gone", "../gone"]);
    std::fs::remove_dir_all(root.path().join("gone")).unwrap();
    git(&repo, &["checkout", "--orphan", "unborn"]);

    let read = read_in_process(&repo).await.unwrap();
    let flags: Vec<_> = (read.iter())
        .map(|tree| (tree.branch.as_deref(), tree.locked, tree.prunable))
        .collect();
    assert_eq!(
        flags,
        [
            (Some("refs/heads/unborn"), false, false),
            (None, false, false),
            (Some("refs/heads/gone"), false, true),
            (Some("refs/heads/held"), true, false),
            (Some("refs/heads/topic"), false, false),
        ]
    );
    assert_read_as_git_lists(&[repo, root.path().join("topic")]).await;
}

#[tokio::test]
async fn reads_relative_worktree_paths_as_git_lists_them() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "worktree.useRelativePaths", "true"]);
    git(&repo, &["worktree", "add", "-b", "topic", "../topic"]);
    git(&repo, &["worktree", "add", "-b", "gone", "../gone"]);
    std::fs::remove_dir_all(root.path().join("gone")).unwrap();
    assert_read_as_git_lists(&[repo]).await;
}

#[tokio::test]
async fn sorts_linked_worktrees_by_path_as_git_does() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    for name in ["b", "C", "a"] {
        git(
            &repo,
            &["worktree", "add", "--detach", &format!("../{name}")],
        );
    }
    for ignore_case in ["false", "true"] {
        git(&repo, &["config", "core.ignoreCase", ignore_case]);
        assert_read_as_git_lists(std::slice::from_ref(&repo)).await;
    }
}

#[tokio::test]
async fn reads_a_bare_repository_and_its_worktrees_as_git_lists_them() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(root.path(), "source");
    let bare = root.path().join("bare.git");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "-q",
            "--",
            source.to_str().unwrap(),
            "bare.git",
        ],
    );
    git(&bare, &["worktree", "add", "../topic", "main"]);
    assert_read_as_git_lists(&[bare, root.path().join("topic")]).await;
}

#[tokio::test]
async fn leaves_a_repository_with_per_worktree_config_to_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "core.repositoryFormatVersion", "1"]);
    git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(read_in_process(&repo).await, None);
    let listed = worktrees(&repo, async |repo, args| Ok(git(repo, args))).await;
    assert_eq!(listed.unwrap(), listed_by_git(&repo));
}

#[test]
fn parses_porcelain_worktree_records() {
    let listing = "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0\
            worktree /repo/.wt/feature\0HEAD def\0branch refs/heads/feature\0locked reason\0prunable gitdir file points to non-existent location\0\0\
            worktree /repo/.wt/detached\0HEAD 123\0detached\0\0";
    let trees = parse_worktrees(listing);
    assert_eq!(trees.len(), 3);
    assert!(trees[0].is_branch(&"main".into()) && !trees[0].locked && !trees[0].prunable);
    assert_eq!(trees[1].path, PathBuf::from("/repo/.wt/feature"));
    assert!(trees[1].locked && trees[1].prunable);
    assert_eq!(trees[2].branch, None);
    assert!(parse_worktrees("").is_empty());
}
