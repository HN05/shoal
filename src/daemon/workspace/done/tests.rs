use super::*;
use crate::{
    forge::pr::Action,
    test_support::{commit, git, manager, repository},
};
use std::{fs, sync::Arc};

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_permit_operations() {
    let (_root, manager, workspace) = fixture().await;
    let resources = manager
        .resource_guard(&workspace.id, GuardMode::Shared)
        .await
        .unwrap();
    manager.mark_done(&workspace.id, Some(false)).await.unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("post_done_cmd = '/usr/bin/true'\n".into()),
        )
        .await
        .unwrap();
    let error = manager
        .mark_done(&workspace.id, Some(true))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("resource operation is in progress")
    );
    assert!(
        !manager
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .cleanup
    );
    drop(resources);
    assert!(
        manager
            .mark_done(&workspace.id, Some(true))
            .await
            .unwrap()
            .cleanup
    );
}

#[tokio::test]
async fn completion_persists_and_explicit_choices_override_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    assert!(
        manager
            .mark_done(&workspace.id, None)
            .await
            .unwrap()
            .cleanup
    );
    manager
        .set_repository_config(&repo.id, Some("[done]\ncleanup=false\n".into()))
        .await
        .unwrap();
    assert!(
        !manager
            .mark_done(&workspace.id, None)
            .await
            .unwrap()
            .cleanup
    );
    assert!(
        manager
            .mark_done(&workspace.id, Some(true))
            .await
            .unwrap()
            .cleanup
    );
    manager
        .set_repository_config(&repo.id, Some("[done]\ncleanup=true\n".into()))
        .await
        .unwrap();
    let kept = manager.mark_done(&workspace.id, Some(false)).await.unwrap();
    assert!(!kept.cleanup);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let completion = reopened
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .completion
        .unwrap();
    assert!(!completion.cleanup);
    assert_eq!(completion.head, kept.head);
    assert!(
        reopened
            .notifications(true, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::WorkspaceDone)
    );
    reopened.store.shutdown().await;
}

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn keep_blocks_idle_and_pr_cleanup_until_explicitly_changed() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager.mark_done(&workspace.id, Some(false)).await.unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        reopened
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.sweep_completed().await.unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    reopened.mark_done(&workspace.id, Some(true)).await.unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
    assert!(reopened.completion(&workspace.id).await.unwrap().is_none());
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn completion_retains_dirty_unpushed_and_newer_work() {
    let (_root, manager, workspace) = fixture().await;
    let file = workspace.path.join("new-work");
    fs::write(&file, "keep").unwrap();
    manager.mark_done(&workspace.id, None).await.unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    commit(&workspace.path, "new-work");
    manager.mark_done(&workspace.id, None).await.unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "neither pushed").await;
    // Simulate a fetched remote-tracking branch preserving the completed HEAD.
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    // A later clean, fully preserved commit must not inherit the old cleanup request.
    fs::write(&file, "newer").unwrap();
    commit(&workspace.path, "new-work");
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    reopened.sweep_completed().await.unwrap();
    assert_error(&reopened, &workspace, "HEAD changed").await;
    assert!(
        reopened
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
    reopened.mark_done(&workspace.id, Some(true)).await.unwrap();
    reopened.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
    // A nonredundant branch is retained even when its commits are pushed.
    let repo = reopened.repository(&workspace.repository_id).await.unwrap();
    git(&repo.path, &["show-ref", "--verify", "refs/heads/finished"]);
    reopened.store.shutdown().await;
}

async fn assert_error(manager: &Manager, workspace: &Workspace, expected: &str) {
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(
        completion.error.as_deref().unwrap().contains(expected),
        "{completion:?}"
    );
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn completion_rechecks_work_after_removal_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let (root, manager, workspace) = fixture().await;
    let hook = root.path().join("modify-work");
    fs::write(&hook, "#!/bin/sh\nprintf 'preserve me' > new-work\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some(format!("pre_remove_cmd = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    manager.mark_done(&workspace.id, None).await.unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("new-work")).unwrap(),
        "preserve me"
    );
}
