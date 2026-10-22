use std::{fs, path::PathBuf, sync::Arc};

use super::*;
use crate::{
    model::{BranchTarget, Repository},
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
        .kept_records(&repo.id)
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

    git(&repo.path, &["push", "-q", "-u", "origin", "topic"]);
    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(!branch_exists(&repo, "topic"));
    assert!(remembered(&manager, &repo).await.is_empty());
    // A branch created later under the name starts without the old upstream.
    let config = git(&repo.path, &["config", "--local", "--list", "--name-only"]);
    assert!(!config.contains("branch.topic."), "{config}");
    assert!(config.contains("branch.main."), "{config}");
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

#[tokio::test]
async fn listing_shows_kept_branches_with_unpushed_commits() {
    let (_root, manager, repo, _other) = fixture().await;
    let head = kept(&manager, &repo, "topic").await;
    kept(&manager, &repo, "pushed").await;
    git(&repo.path, &["push", "-q", "origin", "pushed"]);

    let listed = manager.list_kept_branches().await.unwrap();
    assert_eq!(listed.len(), 1);
    let topic = &listed[0];
    assert_eq!(topic.branch, "topic");
    assert_eq!(topic.repository_id, repo.id);
    assert_eq!(topic.head, head);
    assert_eq!(topic.unpushed_commits, 1);
    assert_eq!(topic.subject, "topic");
}

#[tokio::test]
async fn deletion_checks_every_branch_before_deleting_any() {
    let (root, manager, repo, _other) = fixture().await;
    let mut heads = std::collections::HashMap::new();
    for name in ["first", "second", "busy"] {
        heads.insert(name, kept(&manager, &repo, name).await);
    }
    let target = |branch: &str| BranchTarget {
        repository_id: repo.id.clone(),
        branch: branch.into(),
        head: heads.get(branch).cloned().unwrap_or_default(),
    };
    let listed_earlier = BranchTarget {
        head: heads["first"].clone() + "0",
        ..target("second")
    };
    let elsewhere = root.path().join("elsewhere");
    git(
        &repo.path,
        &["worktree", "add", "-q", elsewhere.to_str().unwrap(), "busy"],
    );

    for (targets, error) in [
        (
            vec![target("first"), target("missing")],
            "missing is not a branch Shoal kept",
        ),
        (vec![target("first"), target("busy")], "busy is checked out"),
        (
            vec![target("first"), listed_earlier.clone()],
            "second changed since it was listed",
        ),
    ] {
        let failed = manager.delete_kept_branches(targets).await.unwrap_err();
        assert!(format!("{failed:#}").contains(error), "{failed:#}");
        assert!(branch_exists(&repo, "first"));
    }

    let deleted = manager
        .delete_kept_branches(vec![target("first"), target("second")])
        .await
        .unwrap();
    let names: Vec<_> = deleted
        .iter()
        .map(|branch| branch.branch.as_str())
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert!(!branch_exists(&repo, "first"));
    assert!(!branch_exists(&repo, "second"));
    assert_eq!(remembered(&manager, &repo).await, ["busy"]);
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
    assert_eq!(
        git(&repo.path, &["for-each-ref", "refs/remotes/mirror/"]),
        ""
    );
}

#[tokio::test]
async fn a_branch_a_bisect_started_from_stays() {
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
    git(&elsewhere, &["bisect", "start"]);
    // Bisecting moves HEAD off the branch it returns to.
    git(&elsewhere, &["switch", "-q", "--detach", "main"]);

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(branch_exists(&repo, "topic"));

    git(&elsewhere, &["bisect", "reset"]);
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

#[tokio::test]
async fn a_kept_branch_made_symbolic_is_forgotten_without_touching_its_target() {
    let (_root, manager, repo, _other) = fixture().await;
    let head = kept(&manager, &repo, "topic").await;
    git(&repo.path, &["branch", "side", &head]);
    git(&repo.path, &["push", "-q", "origin", "side"]);
    git(
        &repo.path,
        &["symbolic-ref", "refs/heads/topic", "refs/heads/side"],
    );

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(remembered(&manager, &repo).await.is_empty());
    assert!(branch_exists(&repo, "side"));
}

#[tokio::test]
async fn a_branch_a_paused_rebase_will_update_stays() {
    let (root, manager, repo, _other) = fixture().await;
    let head = kept(&manager, &repo, "topic").await;
    git(&repo.path, &["push", "-q", "origin", "topic"]);
    let elsewhere = root.path().join("elsewhere");
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stack",
            elsewhere.to_str().unwrap(),
            &head,
        ],
    );
    commit_on(&elsewhere, "stacked");
    // The failing exec pauses the rebase after it reserved topic.
    let paused = std::process::Command::new("git")
        .current_dir(&elsewhere)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["rebase", "--update-refs", "--exec", "false", "main"])
        .output()
        .unwrap();
    assert!(!paused.status.success());
    let common = repo.path.join(".git/worktrees/elsewhere");
    let refs = fs::read_to_string(common.join("rebase-merge/update-refs")).unwrap();
    assert!(refs.contains("refs/heads/topic"), "{refs}");

    manager
        .delete_kept_branches_kept_elsewhere(&repo.id)
        .await
        .unwrap();
    assert!(branch_exists(&repo, "topic"));
    git(&elsewhere, &["rebase", "--abort"]);
}

fn commit_on(path: &std::path::Path, name: &str) {
    fs::write(path.join(name), name).unwrap();
    commit(path, name);
}
