use crate::daemon::agent_service::tests::queue::{
    add, create, launch_in, limit_globally, register,
};
use crate::{
    daemon::{chats::NewChat, manager::Manager, notifications::AlertKind, test_support::manager},
    model::{QueueEntry, Repository},
    removal::{InspectionPolicy, UnsavedWork},
    state::QueueState,
};
use shoal_protocol::{protocol::RetryStart, space_names::SpaceSelector};

/// The entries of the queue with their state.
async fn states(manager: &Manager) -> Vec<(String, QueueState)> {
    (manager
        .agents
        .queue_entries(manager)
        .await
        .unwrap()
        .into_iter())
    .map(|entry| (entry.name.to_string(), entry.state))
    .collect()
}

/// The admitted launch of an agent in a new workspace `name`, failed
/// with `error`.
async fn failed_workspace_launch(manager: &Manager, repo: &Repository, name: &str) -> QueueEntry {
    let entry = add(manager, repo, name).await.entry.unwrap();
    assert_eq!(entry.state, QueueState::Starting);
    (manager
        .agents
        .fail_queue_entry(manager, &entry.id, "the place did not open"))
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

    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
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
        .spaces
        .workspace_by_id(entry.space().unwrap())
        .await
        .unwrap();
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    let told = (notifications.iter())
        .find(|notification| notification.kind == AlertKind::StartFailed)
        .unwrap();
    let number = workspace.number;
    assert!(told.message.contains(&format!("`shoal resume {number}`")));
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
    let entry = manager
        .agents
        .queue_add(&*manager, request)
        .await
        .unwrap()
        .entry
        .unwrap();
    (manager
        .agents
        .fail_queue_entry(&*manager, &entry.id, "gone"))
    .await
    .unwrap();

    let retried = manager
        .agents
        .retry_failed_launch(&*manager, &entry.id, RetryStart::Queued)
        .await
        .unwrap();

    assert_eq!(retried.id, entry.id);
    assert_eq!(retried.mode, Some("plan".into()));
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let plan = manager.agents.queued_plan(&entry.id).await.unwrap().plan;
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
        .spaces
        .workspace_by_id(replaced.space().unwrap())
        .await
        .unwrap();
    manager
        .agents
        .queue_add(&*manager, launch_in(&workspace, false))
        .await
        .unwrap();
    assert_eq!(
        states(&manager).await,
        [("replaced".to_owned(), QueueState::Starting)]
    );

    let removed = failed_workspace_launch(&manager, &repo, "removed").await;
    let selector = SpaceSelector::id(removed.space().unwrap());
    (manager.spaces.remove_selected(
        &*manager,
        &selector,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
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
    let chat = NewChat {
        name: "talk".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
        priority: None,
    };
    let chat = manager
        .spaces
        .start_new_chat(&*manager, Some((&repo.id).into()), None, chat);
    let chat = chat.await.unwrap();
    let launch = crate::protocol::operations::QueuedLaunch::new("claude".into());
    let request = crate::start_plan::queued_agent_request(&chat, launch, None).unwrap();
    let entry = manager
        .agents
        .queue_add(&*manager, request)
        .await
        .unwrap()
        .entry
        .unwrap();

    (manager
        .agents
        .fail_queue_entry(&*manager, &entry.id, "the place did not open"))
    .await
    .unwrap();

    assert!(manager.spaces.chat_by_id(&chat.id).await.unwrap().is_some());
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    let [failed] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_eq!(failed.space(), Some(&chat.id));
    assert_eq!(failed.state, QueueState::Failed);
    let retried = manager
        .agents
        .retry_failed_launch(&*manager, &entry.id, RetryStart::Queued)
        .await
        .unwrap();
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
    let entry = manager
        .agents
        .queue_add(&*manager, request)
        .await
        .unwrap()
        .entry
        .unwrap();
    (manager
        .agents
        .fail_queue_entry(&*manager, &entry.id, "gone"))
    .await
    .unwrap();
    let change = crate::config::permissions::PermissionChange::assigned("merge=direct").unwrap();
    let launches = [SpaceSelector::id(entry.space().unwrap())];
    let targets = crate::daemon::stopped_work::PermissionTargets {
        agents: &[],
        launches: &launches,
    };

    (manager
        .agents
        .change_stopped_permissions(&*manager, targets, &change, None))
    .await
    .unwrap();

    let failed = manager
        .agents
        .queue_entries(&*manager)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(failed.narrowing, change.set);
    let retried = manager
        .agents
        .retry_failed_launch(&*manager, &entry.id, RetryStart::Queued)
        .await
        .unwrap();
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

/// A failed launch restored now starts in the same request, past a full
/// machine, so no admission pass takes it between its requeue and start.
#[tokio::test]
async fn a_failed_launch_restored_now_starts_at_once() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let entry = failed_workspace_launch(&manager, &repo, "work").await;
    add(&manager, &repo, "busy").await;

    let retried = manager
        .agents
        .retry_failed_launch(&*manager, &entry.id, RetryStart::Now)
        .await
        .unwrap();

    assert_eq!(
        (retried.id, retried.state),
        (entry.id, QueueState::Starting)
    );
}

/// The permissions a restore gives its launches change together: a failed
/// launch gone since refuses the change, and the launch waiting for a
/// failed setup beside it keeps what it had.
#[tokio::test]
async fn a_permission_change_refused_for_one_launch_changes_none() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let child = create(&manager, &repo, "child").await;
    let failed = crate::state::WorkspaceState::Failed;
    (manager
        .spaces
        .set_state(&child.id, failed, Some("setup failed".into())))
    .await
    .unwrap();
    let waiting = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap();
    let waiting = waiting.entry.unwrap();
    let given = crate::config::permissions::PermissionChange::assigned("merge=direct")
        .unwrap()
        .set;

    let changed = manager.agents.give_launches(
        vec![(waiting.id.clone(), given)],
        vec![("gone".into(), given)],
    );

    assert!(changed.await.is_err());
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    let kept = entries.iter().find(|entry| entry.id == waiting.id).unwrap();
    assert!(kept.narrowing.is_empty());
}

/// A coordinator changes the permissions of the failed launch of a chat
/// below it, as of a workspace's.
#[tokio::test]
async fn a_coordinator_changes_the_permissions_of_a_failed_chat_below_it() {
    use crate::daemon::scope::Scoped as _;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let parent = crate::daemon::test_support::chat(&manager, "parent", "chat", Some(&repo), None);
    let parent = parent.await;
    let child = NewChat {
        name: "child".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: Some(parent.id.clone()),
        priority: None,
    };
    let child = manager
        .spaces
        .start_new_chat(&*manager, Some((&repo.id).into()), None, child);
    let child = child.await.unwrap();
    let caller = crate::daemon::scope::Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let mut method = crate::protocol::AssignmentMethod::ChangeStoppedPermissions {
        agents: Vec::new(),
        launches: vec![SpaceSelector::id(&child.id)],
        change: crate::config::permissions::PermissionChange::assigned("merge=propose").unwrap(),
    };

    method.scope(&manager, &caller).await.unwrap();
}
