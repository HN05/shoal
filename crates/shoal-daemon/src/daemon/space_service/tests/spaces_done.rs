use crate::daemon::manager::Manager;
use crate::daemon::notifications::AlertKind;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::{
    test_support::{commit, manager, repository},
    workspace_files::Attacher,
};
use crate::model::Workspace;
use std::{fs, sync::Arc};

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn completion_persists_and_cleanup_overrides_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager
        .repositories
        .repository(workspace.repository())
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done::default(),
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled=false\n".into()))
        .await
        .unwrap();
    assert!(
        !manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done::default(),
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    let kept = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(!kept.cleanup);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let completion = reopened
        .spaces
        .inspect_workspace(&*reopened, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .completion
        .unwrap();
    assert!(!completion.cleanup);
    assert_eq!(completion.head, kept.head);
    assert!(
        reopened
            .notifications
            .list(true, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == AlertKind::SpaceDone)
    );
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn repeating_an_unchanged_completion_is_refused() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
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
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap_err();
    // Repeating it closes a thread opened since.
    crate::daemon::test_support::ask(&manager, &workspace.id, "Anything more?").await;
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
    assert_eq!(
        manager
            .coordination
            .waiting_here(&workspace.id)
            .await
            .unwrap(),
        0
    );
    // Another cleanup choice or HEAD replaces the recorded completion.
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    fs::write(workspace.path.join("more-work"), "more").unwrap();
    commit(&workspace.path, "more-work");
    let newer = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .head,
        newer.head
    );
}
