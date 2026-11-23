use crate::model::Workspace;
use crate::stand_in::Fixture;
use crate::state::WorkspaceState;
use crate::workspace::creation::*;

use shoal_protocol::space_names::SpaceSelector;
use std::fs;

/// The one workspace `manager` records.
async fn only_workspace(manager: &Fixture) -> Workspace {
    let workspaces = manager.spaces.list_workspaces().await.unwrap();
    let [workspace] = workspaces.as_slice() else {
        panic!("{workspaces:?}");
    };
    workspace.clone()
}
#[tokio::test]
async fn existing_base_falls_back_only_when_the_default_ref_is_missing() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = crate::stand_in::repository(root.path(), "repo");
    // The cached remote HEAD names the default branch without reading it.
    crate::stand_in::git(&path, &["switch", "-c", "topic"]);
    crate::stand_in::git(&path, &["remote", "add", "origin", "/missing"]);
    crate::stand_in::git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let topic = crate::stand_in::git(&path, &["rev-parse", "topic"]);
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

#[tokio::test]
async fn a_creation_that_fails_after_its_branch_stays_retryable() {
    use crate::stand_in::{block_worktrees, git, repository};
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    assert!(workspace.checkout.is_pending());
    // The branch Git created before the worktree failed is the one a retry
    // checks out.
    assert!(!git(&path, &["branch", "--list", "topic"]).trim().is_empty());
    drop(blocked);
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    assert!(!created.checkout.is_pending());
    let branch = git(&created.path, &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "topic");
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_failed_outside_copy_is_retried_from_its_source() {
    use crate::stand_in::{block_worktrees, git, repository};
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    let failed = manager
        .spaces
        .adopt_workspace(&*manager, &repo.id, &source, true)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    drop(blocked);
    let workspace = (manager.spaces.list_workspaces().await.unwrap().into_iter())
        .find(|workspace| workspace.path != source)
        .unwrap();
    assert!(workspace.checkout.is_pending());
    let selector = SpaceSelector::id(&workspace.id);
    // A retry while the source is gone names it.
    let moved = root.path().join("moved");
    fs::rename(&source, &moved).unwrap();
    let gone = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap_err();
    assert!(format!("{gone:#}").contains("is gone"), "{gone:#}");
    fs::rename(&moved, &source).unwrap();
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    assert_eq!(created.state, WorkspaceState::Ready);
    let copied = fs::read_to_string(created.path.join("unsaved")).unwrap();
    assert_eq!(copied, "copied");
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_retried_outside_copy_keeps_what_the_earlier_try_left() {
    use crate::stand_in::{git, repository};
    use std::os::unix::fs::PermissionsExt;
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    // A file it cannot read fails the copy once Git created the worktree.
    let unreadable = source.join("unreadable");
    fs::write(&unreadable, "secret").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let failed = manager
        .spaces
        .adopt_workspace(&*manager, &repo.id, &source, true)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    let workspace = (manager.spaces.list_workspaces().await.unwrap().into_iter())
        .find(|workspace| workspace.path != source)
        .unwrap();
    assert!(workspace.checkout.git_dir().is_some(), "{workspace:?}");
    fs::write(workspace.path.join("edited"), "by hand").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    let checkpoints = manager
        .spaces
        .checkpoints(&selector, None)
        .await
        .unwrap()
        .unwrap();
    let kept = checkpoints.iter().any(|checkpoint| {
        git(&path, &["show", &format!("{}:edited", checkpoint.commit)]) == "by hand"
    });
    assert!(kept, "{checkpoints:?}");
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_retried_copy_refuses_the_branch_an_earlier_try_made_where_the_source_was() {
    use crate::stand_in::{block_worktrees, commit, git, repository};
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    let blocked = block_worktrees(&path);
    let failed = manager
        .spaces
        .adopt_workspace(&*manager, &repo.id, &source, true)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    drop(blocked);
    let workspace = (manager.spaces.list_workspaces().await.unwrap().into_iter())
        .find(|workspace| workspace.path != source)
        .unwrap();
    let branch = workspace.branch().unwrap().to_string();
    // The source moves on past the commit the earlier try made the branch at.
    fs::write(source.join("later"), "later").unwrap();
    commit(&source, "later");
    let selector = SpaceSelector::id(&workspace.id);
    let refused = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap_err();
    assert!(format!("{refused:#}").contains(&branch), "{refused:#}");
    git(&path, &["branch", "-D", &branch]);
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    let head = git(&created.path, &["rev-parse", "HEAD"]);
    assert_eq!(head, git(&source, &["rev-parse", "HEAD"]));
    manager.store.shutdown().await;
}

#[tokio::test]
async fn a_retried_outside_copy_refuses_a_source_that_moved_past_its_worktree() {
    use crate::stand_in::{commit, git, repository};
    use std::os::unix::fs::PermissionsExt;
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    // A file it cannot read fails the copy once Git created the worktree.
    let unreadable = source.join("unreadable");
    fs::write(&unreadable, "secret").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let failed = manager
        .spaces
        .adopt_workspace(&*manager, &repo.id, &source, true)
        .await;
    assert!(failed.is_err(), "{failed:?}");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(source.join("later"), "later").unwrap();
    commit(&source, "later");
    let workspace = (manager.spaces.list_workspaces().await.unwrap().into_iter())
        .find(|workspace| workspace.path != source)
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let refused = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(!workspace.path.join("later").exists());
    manager.store.shutdown().await;
}
