use shoal_protocol::assignments::Done;
use std::sync::Arc;

use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
        workspace_files::Attacher,
    },
    model::Workspace,
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
