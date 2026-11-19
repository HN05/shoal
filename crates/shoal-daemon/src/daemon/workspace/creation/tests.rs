use super::*;
use shoal_protocol::space_names::SpaceSelector;
use std::fs;

#[tokio::test]
async fn existing_base_falls_back_only_when_the_default_ref_is_missing() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    // The cached remote HEAD names the default branch without reading it.
    crate::daemon::test_support::git(&path, &["switch", "-c", "topic"]);
    crate::daemon::test_support::git(&path, &["remote", "add", "origin", "/missing"]);
    crate::daemon::test_support::git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let topic = crate::daemon::test_support::git(&path, &["rev-parse", "topic"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert_eq!(
        existing_base(&repo, &"topic".into()).await.unwrap(),
        "refs/heads/main"
    );
    let main = path.join(".git/refs/heads/main");
    fs::write(&main, "garbage\n").unwrap();
    assert!(existing_base(&repo, &"topic".into()).await.is_err());
    fs::remove_file(&main).unwrap();
    assert_eq!(
        existing_base(&repo, &"topic".into()).await.unwrap(),
        topic.trim()
    );
    manager.store.shutdown().await;
}

/// The one workspace `manager` records.
async fn only_workspace(manager: &Manager) -> Workspace {
    let workspaces = manager.list_workspaces().await.unwrap();
    let [workspace] = workspaces.as_slice() else {
        panic!("{workspaces:?}");
    };
    workspace.clone()
}

#[tokio::test]
async fn a_creation_that_fails_after_its_branch_stays_retryable() {
    use crate::daemon::test_support::{block_worktrees, git, manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let blocked = block_worktrees(&path);
    let failed = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    let workspace = only_workspace(&manager).await;
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(workspace.pending_worktree);
    // The branch Git created before the worktree failed is the one a retry
    // checks out.
    assert!(!git(&path, &["branch", "--list", "topic"]).trim().is_empty());
    drop(blocked);
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.pending_worktree);
    let branch = git(&created.path, &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "topic");
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_failed_outside_copy_is_retried_from_its_source() {
    use crate::daemon::test_support::{block_worktrees, git, manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let source = root.path().join("outside");
    git(
        &path,
        &["worktree", "add", "-b", "outside", source.to_str().unwrap()],
    );
    let source = fs::canonicalize(source).unwrap();
    fs::write(source.join("unsaved"), "copied").unwrap();
    let blocked = block_worktrees(&path);
    let failed = manager.adopt_workspace(&repo.id, &source, true).await;
    assert!(failed.is_err(), "{failed:?}");
    drop(blocked);
    let workspace = (manager.list_workspaces().await.unwrap().into_iter())
        .find(|workspace| workspace.path != source)
        .unwrap();
    assert!(workspace.pending_worktree);
    let selector = SpaceSelector::id(&workspace.id);
    // A retry while the source is gone names it.
    let moved = root.path().join("moved");
    fs::rename(&source, &moved).unwrap();
    let gone = manager
        .create_pending_worktree(&selector)
        .await
        .unwrap_err();
    assert!(format!("{gone:#}").contains("is gone"), "{gone:#}");
    fs::rename(&moved, &source).unwrap();
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert_eq!(created.state, WorkspaceState::Ready);
    let copied = fs::read_to_string(created.path.join("unsaved")).unwrap();
    assert_eq!(copied, "copied");
    manager.store.shutdown().await;
}
