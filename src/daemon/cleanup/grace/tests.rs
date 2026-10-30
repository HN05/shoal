use std::{fs, sync::Arc};

use crate::{
    daemon::{workspace::Manager, workspace_files::Attacher},
    model::Workspace,
    test_support::{manager, repository},
    time::unix_seconds,
};

/// A workspace whose repository gives cleanup a grace period that cannot
/// end during the test.
async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[cleanup]\ngrace_minutes = 600\n".into()))
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

async fn end_grace(manager: &Manager, workspace: &Workspace) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE cleanup_grace SET cleanup_at=0 WHERE workspace_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn sweep(manager: &Manager) {
    crate::daemon::cleanup::sweep(manager, &mut Default::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn completion_cleanup_starts_the_grace_period_only_once_eligible() {
    let (_root, manager, workspace) = fixture().await;
    let file = workspace.path.join("new-work");
    fs::write(&file, "keep").unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    // Unsaved work is reported at once and does not use up the grace period.
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(completion.error.unwrap().contains("uncommitted"));
    assert_eq!(manager.cleanup_at(&workspace.id).await.unwrap(), None);
    fs::remove_file(&file).unwrap();
    manager.sweep_completed().await.unwrap();
    let cleanup_at = manager.cleanup_at(&workspace.id).await.unwrap().unwrap();
    assert!(cleanup_at >= unix_seconds() + 599 * 60);
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert_eq!(status.cleanup_at, Some(cleanup_at));
    // Later attempts keep the deadline the first one set.
    manager.sweep_completed().await.unwrap();
    assert_eq!(
        manager.cleanup_at(&workspace.id).await.unwrap(),
        Some(cleanup_at)
    );
    assert!(workspace.path.exists());
    end_grace(&manager, &workspace).await;
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn pr_cleanup_waits_for_the_grace_period() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_pr(&workspace.id, crate::daemon::links::pr::Action::Acknowledge)
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(manager.cleanup_at(&workspace.id).await.unwrap().is_some());
    assert!(workspace.path.exists());
    end_grace(&manager, &workspace).await;
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_pass_that_no_longer_removes_the_workspace_ends_its_grace_period() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    sweep(&manager).await;
    assert!(manager.cleanup_at(&workspace.id).await.unwrap().is_some());
    manager.undo_done(&workspace.id).await.unwrap();
    sweep(&manager).await;
    assert_eq!(manager.cleanup_at(&workspace.id).await.unwrap(), None);
    assert!(workspace.path.exists());
    // A later completion starts a new grace period rather than inheriting one.
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    sweep(&manager).await;
    assert!(manager.cleanup_at(&workspace.id).await.unwrap().unwrap() > unix_seconds());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn idle_removal_waits_and_requests_and_disk_space_do_not() {
    let (_root, manager, workspace) = fixture().await;
    let snapshot = manager
        .cleanup_snapshot(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!manager.remove_idle(&workspace.id, snapshot).await.unwrap());
    assert!(manager.cleanup_at(&workspace.id).await.unwrap().is_some());
    assert!(workspace.path.exists());
    manager
        .remove_on_request(&workspace.id, snapshot)
        .await
        .unwrap();
    assert!(!workspace.path.exists());

    let repo = workspace.repository_id.clone();
    let other = manager
        .create_workspace(&repo, "disk".into(), None, None, None)
        .await
        .unwrap();
    let snapshot = manager.cleanup_snapshot(&other.id).await.unwrap().unwrap();
    manager
        .remove_for_disk_space(&other.id, snapshot)
        .await
        .unwrap();
    assert!(!other.path.exists());
}
