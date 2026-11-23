use shoal_protocol::assignments::Done;
use std::{fs, sync::Arc};

use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
        workspace_files::Attacher,
    },
    model::Workspace,
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace whose repository gives cleanup a grace period that cannot
/// end during the test.
async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
                "UPDATE cleanup_grace SET cleanup_at=0 WHERE space_id=?1",
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
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    // Unsaved work is reported at once and does not use up the grace period.
    let completion = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(completion.error.is_some());
    assert_eq!(
        manager.spaces.cleanup_at(&workspace.id).await.unwrap(),
        None
    );
    fs::remove_file(&file).unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    let cleanup_at = manager
        .spaces
        .cleanup_at(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(cleanup_at >= UnixTime::now() + std::time::Duration::from_secs(599 * 60));
    let status = manager
        .spaces
        .space_status(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(status.cleanup_at, Some(cleanup_at));
    // Later attempts keep the deadline the first one set.
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_eq!(
        manager.spaces.cleanup_at(&workspace.id).await.unwrap(),
        Some(cleanup_at)
    );
    assert!(workspace.path.exists());
    end_grace(&manager, &workspace).await;
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn pr_cleanup_waits_for_the_grace_period() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            shoal_protocol::links::pr::Action::Acknowledge,
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(
        manager
            .spaces
            .cleanup_at(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(workspace.path.exists());
    end_grace(&manager, &workspace).await;
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_pass_that_no_longer_removes_the_workspace_ends_its_grace_period() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    sweep(&manager).await;
    assert!(
        manager
            .spaces
            .cleanup_at(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    sweep(&manager).await;
    assert_eq!(
        manager.spaces.cleanup_at(&workspace.id).await.unwrap(),
        None
    );
    assert!(workspace.path.exists());
    // A later completion starts a new grace period rather than inheriting one.
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    sweep(&manager).await;
    assert!(
        manager
            .spaces
            .cleanup_at(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            > UnixTime::now()
    );
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn idle_removal_waits_and_requests_and_disk_space_do_not() {
    let (_root, manager, workspace) = fixture().await;
    let snapshot = manager
        .spaces
        .cleanup_snapshot(&*manager, &workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !manager
            .spaces
            .remove_idle(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
            .await
            .unwrap()
    );
    assert!(
        manager
            .spaces
            .cleanup_at(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(workspace.path.exists());
    manager
        .spaces
        .remove_on_request(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
        .await
        .unwrap();
    assert!(!workspace.path.exists());

    let repo = workspace.repository().clone();
    let other = manager
        .create_workspace(&repo, "disk".into(), None, None, None)
        .await
        .unwrap();
    let snapshot = manager
        .spaces
        .cleanup_snapshot(&*manager, &other.id)
        .await
        .unwrap()
        .unwrap();
    manager
        .spaces
        .remove_for_disk_space(&*manager, &SpaceSelector::id(&other.id), snapshot)
        .await
        .unwrap();
    assert!(!other.path.exists());
}
