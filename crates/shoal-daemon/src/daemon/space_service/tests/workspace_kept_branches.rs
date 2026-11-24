use crate::daemon::manager::Manager;
use std::{fs, path::PathBuf, sync::Arc};

use crate::{
    daemon::test_support::{commit, git, manager, repository},
    model::Repository,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        .spaces
        .remove_selected(
            manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted());
    git(&repo.path, &["rev-parse", name]).trim().to_owned()
}

async fn remembered(manager: &Manager, repo: &Repository) -> Vec<String> {
    manager
        .spaces
        .kept_records(&repo.id)
        .await
        .unwrap()
        .into_iter()
        .map(|kept| kept.branch.into())
        .collect()
}

fn branch_exists(repo: &Repository, name: &str) -> bool {
    !git(&repo.path, &["branch", "--list", name])
        .trim()
        .is_empty()
}

#[tokio::test]
async fn the_sweep_rechecks_a_stalled_checkout_without_workspaces_and_deletes_its_kept_branches() {
    let (_root, manager, repo, _other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    git(&repo.path, &["push", "-q", "-u", "origin", "topic"]);
    // Paused only around the stall, since real Git would let the clock run
    // out its time limit. No workspace is left to recheck the directory.
    tokio::time::pause();
    let stalled = manager
        .file_access
        .checked(
            &manager.notifications,
            &repo.path,
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await;
    tokio::time::advance(crate::daemon::file_access::RECHECK_AFTER).await;
    tokio::time::resume();
    assert!(stalled.is_err());

    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert_eq!(manager.file_access.stalls(), []);
    assert!(!branch_exists(&repo, "topic"));
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
async fn a_stale_ref_of_another_remote_does_not_approve_deletion() {
    let (root, manager, repo, _other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    let mirror = root.path().join("mirror.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            mirror.to_str().unwrap(),
        ],
    );
    git(
        &repo.path,
        &["remote", "add", "mirror", mirror.to_str().unwrap()],
    );
    git(&repo.path, &["push", "-q", "mirror", "topic"]);
    // The mirror drops the branch; only the local ref still says otherwise.
    git(&mirror, &["branch", "-D", "topic"]);

    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(branch_exists(&repo, "topic"));
    assert_eq!(remembered(&manager, &repo).await, ["topic"]);
}
