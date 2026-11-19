use crate::daemon::{
    agent_queue::tests::{create, launch_in, limit_globally, register},
    manager::Manager,
    notifications::NotificationKind,
    test_support::{commit, manager},
};
use crate::model::{QueueEntry, Workspace};

/// Queue the launch of an agent in `workspace` with the queued start
/// `plan`, which the queue admits at once.
async fn starting(manager: &Manager, workspace: &Workspace, plan: &str) -> QueueEntry {
    let request = crate::model::QueueRequest {
        plan: plan.into(),
        ..launch_in(workspace, false)
    };
    manager.queue_add(request).await.unwrap().entry.unwrap()
}

const PLAN: &str = r#"{"launch":{"agent":"claude","prompt":"Fix the login"}}"#;

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
        let entry = starting(&manager, workspace, PLAN).await;
        let opened = manager.open_queued_start(&entry.id, None).await.unwrap();
        assert_eq!(opened.plan, PLAN);
        // The entry is the workspace's, so its agent takes it over.
        let entries = manager.queue_entries().await.unwrap();
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
    let plan = r#"{"launch":{"agent":"claude","prompt":null},"pr":"7"}"#;
    let entry = starting(&manager, &workspace, plan).await;

    manager.open_queued_start(&entry.id, None).await.unwrap();

    let notifications = manager.notifications.list(true, 10).await.unwrap();
    let warned = notifications
        .iter()
        .find(|notification| notification.kind == NotificationKind::StartWarned)
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
    let entry = starting(&manager, &workspace, PLAN).await;
    std::fs::rename(&workspace.path, temp.path().join("elsewhere")).unwrap();

    assert!(manager.open_queued_start(&entry.id, None).await.is_err());
}
