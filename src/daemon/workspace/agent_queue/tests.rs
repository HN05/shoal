use super::*;
use crate::{
    config::Config,
    daemon::agent_queue::tests::STARTED,
    model::Repository,
    test_support::{manager, repository},
};

fn limits(global: Option<u32>, repositories: &[(&str, Option<u32>)]) -> Limits {
    Limits {
        global,
        repositories: repositories
            .iter()
            .map(|(id, limit)| (id.to_string(), *limit))
            .collect(),
    }
}

fn waiting(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(id, repository)| (id.to_string(), repository.to_string()))
        .collect()
}

#[test]
fn admission_fills_free_slots_in_order_and_passes_over_full_repositories() {
    let queue = waiting(&[("a1", "a"), ("a2", "a"), ("b1", "b"), ("b2", "b")]);
    // Repository a may run one agent and already does; b has no limit of its own.
    let limits = limits(Some(3), &[("a", Some(1)), ("b", None)]);
    assert_eq!(admit(&["a".into()], &queue, &limits), ["b1", "b2"]);
    // A full machine admits nothing, whatever the repositories allow.
    assert!(admit(&vec!["b".to_owned(); 3], &queue, &limits).is_empty());
    // Without any limit every entry starts.
    assert_eq!(admit(&[], &queue, &Limits::default()).len(), 4);
}

async fn register(manager: &Manager, temp: &tempfile::TempDir, name: &str) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

fn limit_globally(manager: &Manager, limit: u32) {
    let config: Config = toml::from_str(&format!("max_agents = {limit}")).unwrap();
    manager.publish_config(config);
}

fn request(repository: &Repository, name: &str, now: bool) -> QueueRequest {
    QueueRequest {
        repository: repository.id.as_str().into(),
        name: name.into(),
        agent: "claude".into(),
        issue_title: None,
        plan: "{}".into(),
        env: BTreeMap::from([("HERDR_ENV".into(), "1".into())]),
        now,
    }
}

async fn add(manager: &Manager, repository: &Repository, name: &str) -> QueueAdmission {
    manager
        .queue_add(request(repository, name, false))
        .await
        .unwrap()
}

/// Whether the daemon started `id`, passing the queuing terminal's Herdr variables.
fn started(id: &str) -> bool {
    STARTED.lock().unwrap().iter().any(|(started, env)| {
        started == id && env.get("HERDR_ENV").map(String::as_str) == Some("1")
    })
}

#[tokio::test]
async fn launches_without_a_limit_start_untracked() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let admission = add(&manager, &repo, "first").await;
    assert!(admission.start && admission.entry.is_none());
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_full_machine_queues_until_a_start_ends() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);

    let first = add(&manager, &repo, "first").await;
    let first = first.entry.filter(|_| first.start).unwrap();
    assert_eq!(first.state, QueueState::Starting);
    let second = add(&manager, &repo, "second").await;
    assert!(!second.start);
    assert_eq!(second.waiting, 1);
    let second = second.entry.unwrap();
    assert_eq!(
        (second.state, second.position),
        (QueueState::Queued, Some(1))
    );
    let error = manager
        .queue_add(request(&repo, "second", false))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("already in the agent queue"));
    // Starting now takes a slot beyond the limit without passing the queue.
    let now = manager
        .queue_add(request(&repo, "urgent", true))
        .await
        .unwrap();
    assert!(now.start);

    manager
        .fail_queue_entry(&first.id, "setup failed")
        .await
        .unwrap();
    manager
        .fail_queue_entry(&now.entry.unwrap().id, "gone")
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&second.id));
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, QueueState::Starting);
    let failure = manager.notifications(true, 10).await.unwrap();
    assert!(failure.iter().any(|notification| {
        notification.kind == NotificationKind::QueueFailed
            && notification.message.contains("setup failed")
    }));
    assert!(
        failure
            .iter()
            .any(|notification| notification.kind == NotificationKind::QueueStarted)
    );
}

#[tokio::test]
async fn repository_limits_hold_back_only_their_own_entries() {
    let (temp, manager) = manager().await;
    let limited = register(&manager, &temp, "limited").await;
    let other = register(&manager, &temp, "other").await;
    limit_globally(&manager, 3);
    manager
        .set_repository_config(&limited.id, Some("max_agents = 1\n".into()))
        .await
        .unwrap();

    assert!(add(&manager, &limited, "one").await.start);
    let waiting = add(&manager, &limited, "two").await;
    assert!(!waiting.start);
    assert!(add(&manager, &other, "three").await.start);
    assert!(add(&manager, &other, "four").await.start);
    // The machine is full now, so other repositories wait too.
    assert!(!add(&manager, &other, "five").await.start);
    let positions: Vec<_> = manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|entry| entry.position.map(|position| (entry.name, position)))
        .collect();
    assert_eq!(positions, [("two".into(), 1), ("five".into(), 2)]);
}

#[tokio::test]
async fn completed_workspaces_and_registered_agents_release_starting_slots() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let first = add(&manager, &repo, "first").await.entry.unwrap();
    let second = add(&manager, &repo, "second").await.entry.unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "first".into(), None, None, None)
        .await
        .unwrap();
    manager
        .queue_attach(&first.id, &workspace.id)
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(!started(&second.id));

    manager.mark_done(&workspace.id, Some(false)).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&second.id));

    // An agent registering in the opened workspace takes over its entry.
    manager.consume_queue_entries(&workspace.id).await;
    let names: Vec<_> = manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["second"]);
}

#[tokio::test]
async fn abandoned_starts_expire_after_the_grace_period_unless_held() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let held = add(&manager, &repo, "held").await.entry.unwrap();
    let abandoned = add(&manager, &repo, "abandoned").await.entry.unwrap();
    manager.hold_queue_entry(&held.id).await.unwrap();
    let past = crate::time::unix_seconds() as i64 - START_GRACE.as_secs() as i64 - 1;
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE agent_queue SET seen_at=?1", [past])?;
            Ok(())
        })
        .await
        .unwrap();

    manager.dispatch_queue().await.unwrap();
    let remaining: Vec<_> = manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(remaining, std::slice::from_ref(&held.id));
    assert!(
        manager
            .notifications(true, 10)
            .await
            .unwrap()
            .iter()
            .any(
                |notification| notification.kind == NotificationKind::QueueFailed
                    && notification.message.contains("abandoned")
            )
    );
    assert!(abandoned.state == QueueState::Starting);
    // Releasing the hold starts a fresh grace period instead of ending it.
    manager.release_queue_hold(&held.id).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(manager.queue_entry_starting(&held.id).await.unwrap());
}

#[tokio::test]
async fn only_waiting_entries_can_be_removed() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let starting = add(&manager, &repo, "starting").await.entry.unwrap();
    let waiting = add(&manager, &repo, "waiting").await.entry.unwrap();
    // Only a start can fail; a waiting entry stays queued.
    assert!(
        manager
            .fail_queue_entry(&waiting.id, "not started")
            .await
            .is_err()
    );
    assert_eq!(manager.queue_entries().await.unwrap().len(), 2);

    let error = manager.queue_remove(&starting.name).await.unwrap_err();
    assert!(format!("{error:#}").contains("already starting"));
    let removed = manager.queue_remove("waiting").await.unwrap();
    assert_eq!(removed.name, "waiting");
    assert!(manager.queue_remove("waiting").await.is_err());
    assert!(manager.queued_plan(&starting.id).await.is_ok());
}

#[tokio::test]
async fn each_agent_execution_uses_its_own_slot() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let workspace = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    // Two agents await reattachment in one workspace after a daemon restart.
    manager
        .store
        .run(move |db| {
            for execution in ["first", "second"] {
                db.execute(
                    "INSERT INTO executions(id,workspace_id,state) VALUES (?1,?2,?3)",
                    params![execution, id, ExecutionState::Unknown],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    // A resume record of the same execution is the same agent.
    let record = crate::execution::recovery::record_path(&manager.paths, &workspace.id, "first");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "{\"agent\":\"claude\"}").unwrap();
    assert_eq!(manager.occupied_slots().await.unwrap().len(), 2);
    assert!(!add(&manager, &repo, "waiting").await.start);
}
