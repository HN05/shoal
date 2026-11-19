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

/// A registered repository with a commit past its branch that only a
/// carried ref keeps, as receiving it from another machine leaves it.
async fn carried_here(manager: &Manager, root: &Path) -> (Repository, CommitId) {
    let path = repository(root, "repo");
    let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "carried"];
    let commit = CommitId::from(git(&path, &tree).trim());
    git(
        &path,
        &[
            "update-ref",
            &format!("refs/shoal/carried/{commit}"),
            commit.as_str(),
        ],
    );
    let source = path.to_str().unwrap().into();
    let repo = manager
        .register_repository(source, None, None)
        .await
        .unwrap();
    (repo, commit)
}

/// The commits the carried refs of `repo` keep.
async fn carried(repo: &Repository) -> Vec<CommitId> {
    crate::git::clone_base::carried(&repo.path).await.unwrap()
}

#[tokio::test]
async fn a_failed_start_releases_the_commit_carried_for_it() {
    let (root, manager) = manager().await;
    let (repo, commit) = carried_here(&manager, root.path()).await;
    manager.carried_holds.take(&repo.id, &commit).await;
    // Another machine has no terminal of this one to run an agent in.
    let launch = Launch {
        here: true,
        base: Some(commit.to_string()),
        forwarded: Some(Carried {
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
