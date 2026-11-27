use crate::daemon::{
    agent_service::tests::queue::{claude, create, launch_in, limit_globally, register},
    manager::Manager,
    notifications::AlertKind,
    test_support::{commit, manager},
};
use crate::model::{QueueEntry, QueueRequest, QueuedStart, Workspace};
use crate::protocol::operations::QueuedLaunch;

/// Queue the launch of an agent in `workspace` with the queued start
/// `start`, which the queue admits at once.
async fn starting(manager: &Manager, workspace: &Workspace, start: QueuedStart) -> QueueEntry {
    let request = QueueRequest {
        start,
        ..launch_in(workspace, false)
    };
    manager
        .agents
        .queue_add(manager, request)
        .await
        .unwrap()
        .entry
        .unwrap()
}

/// The start of an agent told to fix the login.
fn fix_the_login() -> QueuedStart {
    let launch = QueuedLaunch {
        prompt: Some("Fix the login".into()),
        ..claude()
    };
    QueuedStart { launch, pr: None }
}

#[tokio::test]
async fn an_admitted_agent_starts_on_its_untouched_branch_brought_up_to_date() {
    let (temp, manager) = manager().await;
    let repository = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let untouched = create(&manager, &repository, "untouched").await;
    let touched = create(&manager, &repository, "touched").await;
    std::fs::write(touched.path.join("own"), "own\n").unwrap();
    commit(&touched.path, "own");
    std::fs::write(repository.path.join("later"), "later\n").unwrap();
    commit(&repository.path, "later");
    for workspace in [&untouched, &touched] {
        let entry = starting(&manager, workspace, fix_the_login()).await;
        let opened = manager
            .agents
            .open_queued_start(&*manager, &entry.id, None)
            .await
            .unwrap();
        assert_eq!(opened.start.launch.prompt.as_deref(), Some("Fix the login"));
        // The entry is the workspace's, so its agent takes it over.
        let entries = manager.agents.queue_entries(&*manager).await.unwrap();
        let taken = entries.iter().find(|queued| queued.id == entry.id).unwrap();
        assert_eq!(taken.space_id.as_ref(), Some(&workspace.id));
    }
    assert!(untouched.path.join("later").exists());
    // Work of its own keeps a branch as it is.
    assert!(!touched.path.join("later").exists());
    assert!(touched.path.join("own").exists());
}

#[tokio::test]
async fn a_pr_the_start_cannot_link_warns_its_starter_and_the_agent_starts() {
    let (temp, manager) = manager().await;
    let repository = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repository, "review").await;
    let pr = Some("7".parse().unwrap());
    let entry = starting(
        &manager,
        &workspace,
        QueuedStart {
            launch: claude(),
            pr,
        },
    )
    .await;

    manager
        .agents
        .open_queued_start(&*manager, &entry.id, None)
        .await
        .unwrap();

    let notifications = manager.notifications.list(true, 10).await.unwrap();
    let warned = notifications
        .iter()
        .find(|notification| notification.kind == AlertKind::StartWarned)
        .expect("the starter was not told of the refused link");
    assert!(
        warned.message.contains("cannot link PR 7"),
        "{}",
        warned.message
    );
}

#[tokio::test]
async fn a_moved_worktree_refuses_its_admitted_agent() {
    let (temp, manager) = manager().await;
    let repository = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repository, "moved").await;
    let entry = starting(&manager, &workspace, fix_the_login()).await;
    std::fs::rename(&workspace.path, temp.path().join("elsewhere")).unwrap();

    assert!(
        manager
            .agents
            .open_queued_start(&*manager, &entry.id, None)
            .await
            .is_err()
    );
}
