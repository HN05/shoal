use std::{fs, path::PathBuf, sync::Arc};

use super::*;
use crate::{
    model::Repository,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, git, manager, repository},
};

/// A registered repository whose `origin` has its main branch, and a clone
/// of that remote for pushing as someone else would.
async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Repository, PathBuf) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        &path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let other = root.path().join("other");
    git(
        root.path(),
        &["clone", "-q", origin.to_str().unwrap(), "other"],
    );
    git(&other, &["config", "user.name", "Shoal Test"]);
    git(&other, &["config", "user.email", "shoal@example.invalid"]);
    (root, manager, repo, other)
}

/// Remove a new workspace on `name` after committing work only it has.
async fn kept(manager: &Manager, repo: &Repository, name: &str) -> String {
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join(name), name).unwrap();
    commit(&workspace.path, name);
    let removed = manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::Retain,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted());
    git(&repo.path, &["rev-parse", name]).trim().to_owned()
}

async fn remembered(manager: &Manager, repo: &Repository) -> Vec<String> {
    manager
        .kept_branches(&repo.id)
        .await
        .unwrap()
        .into_iter()
        .map(|kept| kept.branch)
        .collect()
}

fn branch_exists(repo: &Repository, name: &str) -> bool {
    !git(&repo.path, &["branch", "--list", name])
        .trim()
        .is_empty()
}

#[tokio::test]
async fn a_kept_branch_goes_once_a_remote_has_its_commits() {
    let (_root, manager, repo, _other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    assert_eq!(remembered(&manager, &repo).await, ["topic"]);

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(branch_exists(&repo, "topic"));

    git(&repo.path, &["push", "-q", "origin", "topic"]);
    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(!branch_exists(&repo, "topic"));
    assert!(remembered(&manager, &repo).await.is_empty());
}

#[tokio::test]
async fn a_kept_branch_goes_once_the_default_branch_has_its_changes() {
    let (_root, manager, repo, other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    // The forge squashes the branch onto main; its own commits are never pushed.
    git(
        &other,
        &["fetch", "-q", repo.path.to_str().unwrap(), "topic"],
    );
    git(&other, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    git(&other, &["commit", "-q", "-m", "squash"]);
    git(&other, &["push", "-q", "origin", "main"]);

    // The cleanup sweep fetches before deciding.
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(!branch_exists(&repo, "topic"));
    assert!(remembered(&manager, &repo).await.is_empty());
}

#[tokio::test]
async fn a_moved_reopened_or_deleted_branch_is_forgotten_and_left_alone() {
    let (_root, manager, repo, _other) = fixture().await;
    for name in ["moved", "reopened", "deleted"] {
        kept(&manager, &repo, name).await;
    }
    git(&repo.path, &["branch", "-f", "moved", "main"]);
    let reopened = manager
        .open_branch(&repo.id, "reopened", None, None, None)
        .await
        .unwrap();
    git(&repo.path, &["branch", "-D", "deleted"]);
    // Every remaining one would otherwise be deleted.
    git(&repo.path, &["push", "-q", "origin", "moved", "reopened"]);

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(remembered(&manager, &repo).await.is_empty());
    assert!(branch_exists(&repo, "moved"));
    assert!(branch_exists(&repo, "reopened"));
    assert!(reopened.workspace.path.is_dir());
}

#[tokio::test]
async fn a_branch_checked_out_elsewhere_stays_remembered() {
    let (root, manager, repo, _other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    git(&repo.path, &["push", "-q", "origin", "topic"]);
    let elsewhere = root.path().join("elsewhere");
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-q",
            elsewhere.to_str().unwrap(),
            "topic",
        ],
    );

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(branch_exists(&repo, "topic"));
    assert_eq!(remembered(&manager, &repo).await, ["topic"]);

    git(
        &repo.path,
        &["worktree", "remove", elsewhere.to_str().unwrap()],
    );
    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(!branch_exists(&repo, "topic"));
}
