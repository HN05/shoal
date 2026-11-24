use crate::daemon::manager::Manager;
use crate::model::EventCause;
use crate::model::Space;
use crate::model::Workspace;

use crate::daemon::{
    links::pr::Action,
    test_support::{manager, repository},
    workspace_files::Attacher,
};
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

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
async fn completion_without_cleanup_blocks_idle_and_pr_cleanup_until_requested() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
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
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        reopened
            .spaces
            .cleanup_snapshot(&*reopened, &workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.spaces.sweep_completed(&*reopened).await.unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    reopened
        .spaces
        .mark_space_done(
            &*reopened,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
    assert!(
        reopened
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn undone_withdraws_completion_without_blocking_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    manager
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
    let withdrawn = manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(withdrawn.cleanup);
    manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap_err();
    assert!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    // Without completion, links or holds the workspace is an idle candidate again.
    assert!(
        manager
            .spaces
            .cleanup_snapshot(&*manager, &workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    // A hold, not the withdrawn completion, keeps a merged workspace.
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn issue_and_watch_completion_wait_for_explicit_done() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    let record = serde_json::json!({
        "urls": ["https://github.com/team/repo/pull/1"],
        "head": null,
        "error": null,
    })
    .to_string();
    manager
        .store
        .run(move |db| {
            // Without an origin, any issue or PR lookup would record an error.
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, record],
            )?;
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                rusqlite::params![id, "https://github.com/team/repo/issues/1"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(inspection.completion.is_none());
    assert!(inspection.issues[0].error.is_none());
    assert!(inspection.pr_cleanup.unwrap().error.is_none());
    // Explicit completion applies the watch's merge checks.
    manager
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
    manager.sweep_prs().await.unwrap();
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(inspection.pr_cleanup.unwrap().error.is_some());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn threads_asked_after_done_retain_completion_and_pr_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    // Done closes the threads opened before it.
    crate::daemon::test_support::ask(&manager, &workspace.id, "Anything else?").await;
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
    let question =
        crate::daemon::test_support::ask(&manager, &workspace.id, "Anything more?").await;
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(workspace.path.exists());
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    let completion = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(completion.error.is_none());
    // A thread opened after a sweep checked for one still stops its removal.
    manager
        .spaces
        .remove_completed(
            &*manager,
            &Space::from(workspace.clone()),
            &completion,
            EventCause::Completion,
        )
        .await
        .unwrap_err();
    manager
        .spaces
        .remove_merged(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &completion.head,
        )
        .await
        .unwrap_err();
    assert!(workspace.path.exists());
    assert_eq!(
        manager
            .coordination
            .waiting_here(&workspace.id)
            .await
            .unwrap(),
        1
    );
    crate::daemon::test_support::answer(&manager, question.number, "No").await;
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_worker_opened_outside_its_done_chat_after_the_sweep_chose_it_stays() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let worker = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repo.id.into(),
            "worker".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    let ancestor = manager
        .spaces
        .completed_ancestor(&*manager, &worker.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ancestor.id, chat.id);

    // The user opens it outside the chat before the removal reserves it.
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_under_completed(&*manager, &Space::from(worker.clone()), &ancestor)
        .await
        .unwrap_err();
    assert!(manager.spaces.workspace_by_id(&worker.id).await.is_ok());
}

#[tokio::test]
async fn a_removal_under_a_done_chat_keeps_the_chats_workspaces_below_it() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let root = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repo.id.into(),
            "root".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let split = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&root.id),
            "split".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    let split = manager.spaces.workspace_by_id(&split.id).await.unwrap();
    let ancestor = manager
        .spaces
        .completed_ancestor(&*manager, &split.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ancestor.id, chat.id);

    // While a removal under the chat holds its workspaces, the root that
    // puts the split child below the chat stays there...
    let held = manager.spaces.completed_owners.reserve(&chat.id).unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &root.id, None)
        .await
        .unwrap_err();
    // ...and the removal takes that hold itself, so another one waits.
    manager
        .spaces
        .remove_under_completed(&*manager, &Space::from(split.clone()), &ancestor)
        .await
        .unwrap_err();
    assert!(manager.spaces.workspace_by_id(&split.id).await.is_ok());
    drop(held);
    manager
        .spaces
        .assign_parent(&*manager, &root.id, None)
        .await
        .unwrap();
}
