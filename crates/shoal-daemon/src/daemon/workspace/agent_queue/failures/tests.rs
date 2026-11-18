use super::super::tests::{add, create, launch_in, limit_globally, register};
use crate::{
    daemon::{
        chats::NewChat, notifications::NotificationKind, test_support::manager, workspace::Manager,
    },
    model::{ChatQueueRequest, QueueEntry, Repository},
    removal::{InspectionPolicy, UnsavedWork},
    state::QueueState,
};
use shoal_protocol::space_names::SpaceSelector;

/// The entries of the queue with their state.
async fn states(manager: &Manager) -> Vec<(String, QueueState)> {
    (manager.queue_entries().await.unwrap().into_iter())
        .map(|entry| (entry.name.to_string(), entry.state))
        .collect()
}

/// The admitted launch of an agent in a new workspace `name`, failed
/// with `error`.
async fn failed_workspace_launch(manager: &Manager, repo: &Repository, name: &str) -> QueueEntry {
    let entry = add(manager, repo, name).await.entry.unwrap();
    assert_eq!(entry.state, QueueState::Starting);
    (manager.fail_queue_entry(&entry.id, "the place did not open"))
        .await
        .unwrap();
    entry
}

/// A launch whose start fails stays on its workspace as a failed entry
/// with its error, which admission passes over, and its starter hears of
/// it with what starts it again and what removes the workspace.
#[tokio::test]
async fn a_failed_launch_stays_on_its_space_with_its_error() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let entry = failed_workspace_launch(&manager, &repo, "work").await;

    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    let [failed] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_eq!((&failed.id, failed.state), (&entry.id, QueueState::Failed));
    assert_eq!(failed.error.as_deref(), Some("the place did not open"));
    assert_eq!(failed.space(), entry.space());
    // It holds no slot.
    let next = add(&manager, &repo, "next").await.entry.unwrap();
    assert_eq!(next.state, QueueState::Starting);
    let workspace = manager
        .workspace_by_id(entry.space().unwrap())
        .await
        .unwrap();
    let notifications = manager.notifications(true, 10).await.unwrap();
    let told = (notifications.iter())
        .find(|notification| notification.kind == NotificationKind::StartFailed)
        .unwrap();
    let number = workspace.number;
    assert!(told.message.contains(&format!("`shoal restore {number}`")));
    assert!(told.message.contains(&format!("`shoal rm {number}`")));
}

/// Restoring a failed launch queues it again as it was, under its own
/// entry, and the queue then starts it.
#[tokio::test]
async fn a_restored_failed_launch_queues_again_as_it_was() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "work").await;
    let request = crate::model::QueueRequest {
        plan: r#"{"launch":{"agent":"claude","prompt":"Fix the login"}}"#.into(),
        mode: Some("plan".into()),
        ..launch_in(&workspace, false)
    };
    let entry = manager.queue_add(request).await.unwrap().entry.unwrap();
    (manager.fail_queue_entry(&entry.id, "gone")).await.unwrap();

    let retried = manager.retry_failed_launch(&entry.id, false).await.unwrap();

    assert_eq!(retried.id, entry.id);
    assert_eq!(retried.mode, Some("plan".into()));
    manager.dispatch_queue().await.unwrap();
    let plan = manager.queued_plan(&entry.id).await.unwrap().plan;
    assert!(plan.contains("Fix the login"), "{plan}");
}

/// A new launch in the space replaces its failed one, and removing the
/// space drops it.
#[tokio::test]
async fn a_new_launch_or_the_spaces_removal_ends_its_failed_launch() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let replaced = failed_workspace_launch(&manager, &repo, "replaced").await;
    let workspace = manager
        .workspace_by_id(replaced.space().unwrap())
        .await
        .unwrap();
    manager
        .queue_add(launch_in(&workspace, false))
        .await
        .unwrap();
    assert_eq!(
        states(&manager).await,
        [("replaced".to_owned(), QueueState::Starting)]
    );

    let removed = failed_workspace_launch(&manager, &repo, "removed").await;
    let selector = SpaceSelector::id(removed.space().unwrap());
    (manager.remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly))
        .await
        .unwrap();
    let names: Vec<_> = states(&manager)
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["replaced"]);
}

/// A chat whose launch fails stays, with its failed launch, as a workspace
/// does, and restoring it starts its agent from the queue.
#[tokio::test]
async fn a_chats_failed_launch_keeps_the_chat() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let request = ChatQueueRequest {
        source: None,
        repository: Some((&repo.id).into()),
        name: "talk".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        plan: r#"{"launch":{"agent":"claude"}}"#.into(),
        env: Default::default(),
        now: false,
        priority: None,
    };
    let entry = manager.queue_chat_add(request, None).await.unwrap();
    let entry = entry.entry.unwrap();
    let chat = NewChat {
        name: "talk".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
        priority: None,
    };
    let chat = manager.start_new_chat(Some((&repo.id).into()), None, chat);
    let chat = chat.await.unwrap();
    (manager.queue_attach(&entry.id, &SpaceSelector::id(&chat.id)))
        .await
        .unwrap();

    (manager.fail_queue_entry(&entry.id, "the place did not open"))
        .await
        .unwrap();

    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
    let entries = manager.queue_entries().await.unwrap();
    let [failed] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_eq!(failed.space(), Some(&chat.id));
    assert_eq!(failed.state, QueueState::Failed);
    let retried = manager.retry_failed_launch(&entry.id, false).await.unwrap();
    assert_eq!(retried.space(), Some(&chat.id));
}

/// A permission change a restore makes reaches a failed launch: the entry
/// the queue lists and the plan its agent starts under once restored.
#[tokio::test]
async fn a_failed_launch_takes_a_permission_change() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "work").await;
    let request = crate::model::QueueRequest {
        plan: r#"{"launch":{"agent":"claude"}}"#.into(),
        ..launch_in(&workspace, false)
    };
    let entry = manager.queue_add(request).await.unwrap().entry.unwrap();
    (manager.fail_queue_entry(&entry.id, "gone")).await.unwrap();
    let change = crate::config::permissions::PermissionChange::assigned("merge=direct").unwrap();
    let launches = [SpaceSelector::id(entry.space().unwrap())];
    let targets = crate::daemon::workspace::PermissionTargets {
        agents: &[],
        launches: &launches,
    };

    (manager.change_stopped_permissions(targets, &change, None))
        .await
        .unwrap();

    let failed = manager.queue_entries().await.unwrap().remove(0);
    assert_eq!(failed.narrowing, change.set);
    let retried = manager.retry_failed_launch(&entry.id, false).await.unwrap();
    assert_eq!(retried.narrowing, change.set);
    let id = retried.id.clone();
    let plan: String = (manager.store)
        .read(move |tx| {
            let plan = "SELECT plan FROM agent_queue WHERE id=?1";
            Ok(tx.query_row(plan, [id], |row| row.get(0))?)
        })
        .await
        .unwrap();
    let start = crate::start_plan::QueuedStart::of(&plan).unwrap();
    assert_eq!(start.launch.narrowing, change.set);
}
