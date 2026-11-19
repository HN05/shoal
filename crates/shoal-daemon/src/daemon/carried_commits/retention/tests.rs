use std::path::Path;

use shoal_protocol::launches::{Carried, Launch, LaunchSource};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

use crate::{
    daemon::{
        manager::Manager,
        test_support::{block_worktrees, git, manager, repository},
    },
    model::Repository,
    removal::{InspectionPolicy, UnsavedWork},
};

/// The origin a start carries its commit from in these tests.
const ORIGIN: &str = "https://forge.example/owner/repo.git";

/// A new commit past the branch of the repository at `path`, which no
/// branch holds.
fn unreferenced_commit(path: &Path) -> CommitId {
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "carried"];
    CommitId::from(git(path, &tree).trim())
}

/// Keep `commit` of the repository at `path` under its carried ref, as
/// receiving it from another machine does.
fn carry(path: &Path, commit: &CommitId) {
    let reference = format!("refs/shoal/carried/{commit}");
    git(path, &["update-ref", &reference, commit.as_str()]);
}

async fn register(manager: &Manager, path: &Path) -> Repository {
    let source = path.to_str().unwrap().into();
    (manager.register_repository(source, None, None))
        .await
        .unwrap()
}

/// A registered repository with a commit past its branch that only a
/// carried ref keeps.
async fn carried_here(manager: &Manager, root: &Path) -> (Repository, CommitId) {
    let path = repository(root, "repo");
    let commit = unreferenced_commit(&path);
    carry(&path, &commit);
    (register(manager, &path).await, commit)
}

/// The commits the carried refs of `repo` keep.
async fn carried(repo: &Repository) -> Vec<CommitId> {
    crate::git::clone_base::carried(&repo.path).await.unwrap()
}

#[tokio::test]
async fn a_failed_start_releases_the_commit_carried_for_it() {
    let (root, manager) = manager().await;
    let (repo, commit) = carried_here(&manager, root.path()).await;
    manager
        .carried_holds
        .take(&repo.id, (ORIGIN, &commit))
        .await;
    // Another machine has no terminal of this one to run an agent in.
    let launch = Launch {
        here: true,
        base: Some(commit.to_string()),
        forwarded: Some(Carried {
            origin: Some(ORIGIN.into()),
            commit: Some(commit.clone()),
            ..Carried::default()
        }),
        ..Launch::default()
    };

    let failed = manager.launch(launch, LaunchSource::Manual, None).await;

    assert!(failed.is_err(), "{failed:?}");
    assert!(carried(&repo).await.is_empty());
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_workspace_keeps_its_carried_base_until_its_branch_holds_it() {
    let (root, manager) = manager().await;
    let (repo, commit) = carried_here(&manager, root.path()).await;
    let blocked = block_worktrees(&repo.path);
    let base = Some(commit.to_string());
    let failed = (manager.create_workspace(&repo.id, "topic".into(), base, None, None)).await;
    assert!(failed.is_err(), "{failed:?}");
    drop(blocked);

    // The retry of its creation starts from it.
    manager.release_carried(&repo.id).await;
    assert_eq!(carried(&repo).await, std::slice::from_ref(&commit));
    let selector = manager.named("topic").await;
    let created = manager.create_pending_worktree(&selector).await.unwrap();

    assert!(carried(&repo).await.is_empty());
    assert_eq!(
        git(&created.path, &["rev-parse", "HEAD"]).trim(),
        commit.as_str()
    );
    manager.store.shutdown().await;
}

#[tokio::test]
async fn removing_the_workspace_that_waited_for_a_carried_base_releases_it() {
    let (root, manager) = manager().await;
    let (repo, commit) = carried_here(&manager, root.path()).await;
    let blocked = block_worktrees(&repo.path);
    let base = Some(commit.to_string());
    let failed = (manager.create_workspace(&repo.id, "topic".into(), base, None, None)).await;
    assert!(failed.is_err(), "{failed:?}");
    drop(blocked);
    let selector: SpaceSelector = manager.named("topic").await;

    (manager.remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly))
        .await
        .unwrap();

    assert!(carried(&repo).await.is_empty());
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_start_releases_only_the_hold_of_the_repository_it_started_in() {
    let (root, manager) = manager().await;
    let first = repository(root.path(), "first");
    let commit = unreferenced_commit(&first);
    git(&first, &["branch", "carried", commit.as_str()]);
    let second = root.path().join("second");
    let clone = [
        "clone",
        "-q",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
    ];
    git(root.path(), &clone);
    git(&second, &["remote", "remove", "origin"]);
    git(&first, &["branch", "-D", "carried"]);
    let (first, second) = (
        register(&manager, &first).await,
        register(&manager, &second).await,
    );
    for repo in [&first, &second] {
        carry(&repo.path, &commit);
    }
    let other = "https://forge.example/fork/repo.git";
    manager
        .carried_holds
        .take(&first.id, (ORIGIN, &commit))
        .await;
    manager
        .carried_holds
        .take(&second.id, (other, &commit))
        .await;

    manager.release_carried_hold((other, &commit)).await;

    assert!(carried(&second).await.is_empty());
    assert_eq!(carried(&first).await, std::slice::from_ref(&commit));
    manager.release_carried_hold((ORIGIN, &commit)).await;
    assert!(carried(&first).await.is_empty());
    manager.store.shutdown().await;
}

#[tokio::test]
async fn the_first_sweep_releases_the_refs_a_restart_left_unheld() {
    let (root, manager) = manager().await;
    let (repo, _) = carried_here(&manager, root.path()).await;

    manager.release_unclaimed_carried().await.unwrap();

    assert!(carried(&repo).await.is_empty());
    manager.store.shutdown().await;
}
