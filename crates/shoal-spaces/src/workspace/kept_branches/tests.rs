use crate::stand_in::Fixture;
use shoal_core::test_support::{commit, git, repository};
use std::{fs, path::PathBuf};

use crate::{
    model::{BranchTarget, Repository},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

/// A registered repository whose `origin` has its main branch, and a clone
/// of that remote for pushing as someone else would.
async fn fixture() -> (Fixture, Repository, PathBuf) {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    (manager, repo, other)
}

/// Remove a new workspace on `name` after committing work only it has.
async fn kept(manager: &Fixture, repo: &Repository, name: &str) -> String {
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join(name), name).unwrap();
    commit(&workspace.path, name);
    let removed = manager
        .spaces
        .remove_selected(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted());
    git(&repo.path, &["rev-parse", name]).trim().to_owned()
}

async fn remembered(manager: &Fixture, repo: &Repository) -> Vec<String> {
    manager
        .spaces
        .kept_records(&repo.id)
        .await
        .unwrap()
        .into_iter()
        .map(|kept| kept.branch.into())
        .collect()
}

/// One kept-branch pass with what the remotes advertise now.
async fn sweep_kept(manager: &Fixture, repo: &Repository) {
    let heads = manager
        .spaces
        .remote_heads(&**manager, &repo.id)
        .await
        .unwrap();
    manager
        .spaces
        .delete_kept_branches_kept_elsewhere(&**manager, &repo.id, &heads)
        .await
        .unwrap();
}

fn branch_exists(repo: &Repository, name: &str) -> bool {
    !git(&repo.path, &["branch", "--list", name])
        .trim()
        .is_empty()
}

fn commit_on(path: &std::path::Path, name: &str) {
    fs::write(path.join(name), name).unwrap();
    commit(path, name);
}
#[tokio::test]
async fn a_kept_branch_goes_once_a_remote_has_its_commits() {
    let (manager, repo, _other) = fixture().await;
    kept(&manager, &repo, "topic").await;
    assert_eq!(remembered(&manager, &repo).await, ["topic"]);

    sweep_kept(&manager, &repo).await;
    assert!(branch_exists(&repo, "topic"));

    git(&repo.path, &["push", "-q", "-u", "origin", "topic"]);
    sweep_kept(&manager, &repo).await;
    assert!(!branch_exists(&repo, "topic"));
    assert!(remembered(&manager, &repo).await.is_empty());
    // A branch created later under the name starts without the old upstream.
    let config = git(&repo.path, &["config", "--local", "--list", "--name-only"]);
    assert!(!config.contains("branch.topic."), "{config}");
    assert!(config.contains("branch.main."), "{config}");
}

#[tokio::test]
async fn a_moved_reopened_or_deleted_branch_is_forgotten_and_left_alone() {
    let (manager, repo, _other) = fixture().await;
    for name in ["moved", "reopened", "deleted"] {
        kept(&manager, &repo, name).await;
    }
    git(&repo.path, &["branch", "-f", "moved", "main"]);
    let reopened = manager
        .spaces
        .open_branch(&*manager, &repo.id, "reopened", None, None, None)
        .await
        .unwrap();
    git(&repo.path, &["branch", "-D", "deleted"]);
    // Every remaining one would otherwise be deleted.
    git(&repo.path, &["push", "-q", "origin", "moved", "reopened"]);

    sweep_kept(&manager, &repo).await;
    assert!(remembered(&manager, &repo).await.is_empty());
    assert!(branch_exists(&repo, "moved"));
    assert!(branch_exists(&repo, "reopened"));
    assert!(reopened.workspace.path.is_dir());
}

#[tokio::test]
async fn a_branch_checked_out_elsewhere_stays_remembered() {
    let (manager, repo, _other) = fixture().await;
    let root = &manager.root;
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

    sweep_kept(&manager, &repo).await;
    assert!(branch_exists(&repo, "topic"));
    assert_eq!(remembered(&manager, &repo).await, ["topic"]);

    git(
        &repo.path,
        &["worktree", "remove", elsewhere.to_str().unwrap()],
    );
    sweep_kept(&manager, &repo).await;
    assert!(!branch_exists(&repo, "topic"));
}

#[tokio::test]
async fn listing_shows_every_kept_branch_with_its_unpushed_commits() {
    let (manager, repo, _other) = fixture().await;
    let head = kept(&manager, &repo, "topic").await;
    kept(&manager, &repo, "pushed").await;
    git(&repo.path, &["push", "-q", "origin", "pushed"]);

    let listed = manager
        .spaces
        .list_kept_branches(&*manager, None)
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    let pushed = listed.iter().find(|branch| branch.branch == "pushed");
    assert_eq!(pushed.map(|branch| branch.unpushed_commits), Some(0));
    let topic = listed
        .iter()
        .find(|branch| branch.branch == "topic")
        .expect("topic is listed");
    assert_eq!(topic.repository_id, repo.id);
    assert_eq!(topic.head, head);
    assert_eq!(topic.unpushed_commits, 1);
    assert_eq!(topic.subject, "topic");
}

#[tokio::test]
async fn deletion_checks_every_branch_before_deleting_any() {
    let (manager, repo, _other) = fixture().await;
    let root = &manager.root;
    let mut heads = std::collections::HashMap::new();
    for name in ["first", "second", "busy"] {
        heads.insert(name, kept(&manager, &repo, name).await);
    }
    let target = |branch: &str| BranchTarget {
        repository_id: repo.id.clone(),
        branch: branch.into(),
        head: heads.get(branch).cloned().unwrap_or_default().into(),
    };
    let listed_earlier = BranchTarget {
        head: (heads["first"].clone() + "0").into(),
        ..target("second")
    };
    let elsewhere = root.path().join("elsewhere");
    git(
        &repo.path,
        &["worktree", "add", "-q", elsewhere.to_str().unwrap(), "busy"],
    );

    for targets in [
        vec![target("first"), target("missing")],
        vec![target("first"), target("busy")],
        vec![target("first"), listed_earlier.clone()],
    ] {
        manager
            .spaces
            .delete_kept_branches(&*manager, targets)
            .await
            .unwrap_err();
        assert!(branch_exists(&repo, "first"));
    }

    let deleted = manager
        .spaces
        .delete_kept_branches(&*manager, vec![target("first"), target("second")])
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
async fn a_branch_a_bisect_started_from_stays() {
    let (manager, repo, _other) = fixture().await;
    let root = &manager.root;
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

    sweep_kept(&manager, &repo).await;
    assert!(branch_exists(&repo, "topic"));

    git(&elsewhere, &["bisect", "reset"]);
    git(
        &repo.path,
        &["worktree", "remove", elsewhere.to_str().unwrap()],
    );
    sweep_kept(&manager, &repo).await;
    assert!(!branch_exists(&repo, "topic"));
}

#[tokio::test]
async fn a_kept_branch_made_symbolic_is_forgotten_without_touching_its_target() {
    let (manager, repo, _other) = fixture().await;
    let head = kept(&manager, &repo, "topic").await;
    git(&repo.path, &["branch", "side", &head]);
    git(&repo.path, &["push", "-q", "origin", "side"]);
    git(
        &repo.path,
        &["symbolic-ref", "refs/heads/topic", "refs/heads/side"],
    );

    sweep_kept(&manager, &repo).await;
    assert!(remembered(&manager, &repo).await.is_empty());
    assert!(branch_exists(&repo, "side"));
}

#[tokio::test]
async fn a_branch_a_paused_rebase_will_update_stays() {
    let (manager, repo, _other) = fixture().await;
    let root = &manager.root;
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

    sweep_kept(&manager, &repo).await;
    assert!(branch_exists(&repo, "topic"));
    git(&elsewhere, &["rebase", "--abort"]);
}
