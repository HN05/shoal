use super::*;
use crate::{
    daemon::agent_queue::tests::STARTED,
    model::Repository,
    test_support::{global_config, manager, repository},
};

fn limits(global: Option<u32>, repositories: &[(&str, Option<u32>)]) -> Limits {
    Limits {
        global,
        starts: None,
        repositories: repositories
            .iter()
            .map(|(id, limit)| (id.to_string(), *limit))
            .collect(),
        disk: None,
    }
}

fn waiting(entries: &[(&str, &str)]) -> Vec<Waiting> {
    entries
        .iter()
        .map(|(id, repository)| Waiting {
            id: id.to_string(),
            repository: repository.to_string(),
            work: if id.starts_with('r') {
                Work::Restore { waiting: true }
            } else {
                Work::Launch
            },
        })
        .collect()
}

#[test]
fn admission_fills_free_slots_in_order_and_passes_over_full_repositories() {
    let queue = waiting(&[("a1", "a"), ("a2", "a"), ("b1", "b"), ("b2", "b")]);
    // Repository a may run one agent and already does; b has no limit of its own.
    let limits = limits(Some(3), &[("a", Some(1)), ("b", None)]);
    assert_eq!(admit(&["a".into()], &queue, &limits, false), ["b1", "b2"]);
    // A full machine admits nothing, whatever the repositories allow.
    assert!(admit(&vec!["b".to_owned(); 3], &queue, &limits, false).is_empty());
    // Without any limit every entry starts.
    assert_eq!(admit(&[], &queue, &Limits::default(), false).len(), 4);
    // Low disk holds the machine below its fixed limit.
    let low_disk = Limits {
        disk: Some(2),
        ..limits
    };
    assert_eq!(admit(&["a".into()], &queue, &low_disk, false), ["b1"]);
    // Measured load starts as many as it allows, whatever the slots in use.
    let dynamic = Limits {
        starts: Some(1),
        ..Limits::default()
    };
    assert_eq!(
        admit(&vec!["b".to_owned(); 9], &queue, &dynamic, false),
        ["a1"]
    );
}

#[test]
fn restores_take_back_their_own_slot_under_fixed_limits() {
    // Restores are named r…; each one's stopped agent holds a slot in `occupied`.
    let queue = waiting(&[("a1", "a"), ("ra", "a"), ("b1", "b")]);
    let occupied = |repositories: &[&str]| -> Vec<String> {
        repositories.iter().map(|id| id.to_string()).collect()
    };
    // The machine is full: the launch waits and the restore takes its slot back.
    let full = limits(Some(3), &[("a", Some(2))]);
    assert_eq!(
        admit(&occupied(&["a", "a", "b"]), &queue, &full, true),
        ["ra"]
    );
    // A lowered machine limit holds the restore until running agents leave.
    let lowered = limits(Some(2), &[]);
    assert!(admit(&occupied(&["a", "b", "b"]), &queue, &lowered, true).is_empty());
    assert_eq!(
        admit(&occupied(&["a", "b"]), &queue, &lowered, true),
        ["ra"]
    );
    // So does a lowered repository limit, which holds back only its own entries.
    let lowered = limits(Some(4), &[("a", Some(1))]);
    assert_eq!(
        admit(&occupied(&["a", "a"]), &queue, &lowered, true),
        ["b1"]
    );
    // Low disk holds the slots in use, which a restore already has.
    let low_disk = Limits {
        disk: Some(3),
        ..Limits::default()
    };
    assert_eq!(
        admit(&occupied(&["a", "a", "b"]), &queue, &low_disk, true),
        ["ra"]
    );
}

#[test]
fn restores_wait_for_healthy_readings_and_hold_back_later_entries() {
    let mut queue = waiting(&[("a1", "a"), ("ra", "a"), ("rb", "b"), ("b1", "b")]);
    let occupied = vec!["a".to_owned(), "b".to_owned()];
    let unlimited = Limits::default();
    // Without healthy readings only the launch ahead of the restores starts.
    assert_eq!(admit(&occupied, &queue, &unlimited, false), ["a1"]);
    // Each healthy interval restores one agent; the rest keep their place.
    assert_eq!(admit(&occupied, &queue, &unlimited, true), ["a1", "ra"]);
    // A restore whose agent is not waiting yet is passed over.
    queue[1].work = Work::Restore { waiting: false };
    assert_eq!(
        admit(&occupied, &queue, &unlimited, true),
        ["a1", "rb", "b1"]
    );
    // Dynamic admission measures room for a restored agent's load too.
    let starts = |starts| Limits {
        starts: Some(starts),
        ..Limits::default()
    };
    assert_eq!(admit(&occupied, &queue[1..], &starts(1), true), ["rb"]);
    assert!(admit(&occupied, &queue[1..], &starts(0), true).is_empty());
}

async fn register(manager: &Manager, temp: &tempfile::TempDir, name: &str) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// Limit the machine to `limit` agents; host disk must not decide the test.
fn limit_globally(manager: &Manager, limit: u32) {
    manager.publish_config(global_config(&format!(
        "max_agents = {limit}\n[overload.disk]\nenabled = false"
    )));
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
        workspace: None,
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
    manager.publish_config(global_config("max_agents = 'none'"));
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

    manager.mark_done(&workspace.id, false).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&second.id));

    // Only the queued launch's own agent takes over its entry.
    manager.consume_queue_entry(&second.id, &workspace.id).await;
    assert_eq!(manager.queue_entries().await.unwrap().len(), 2);
    manager.consume_queue_entry(&first.id, &workspace.id).await;
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

#[tokio::test]
async fn dynamic_admission_staggers_starts() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Host memory and disk must not decide this test.
    manager.publish_config(global_config(
        "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false",
    ));
    // An idle machine starts the first agent whatever its CPU reads.
    assert!(add(&manager, &repo, "first").await.start);
    // The next waits for the stagger even with room.
    let second = add(&manager, &repo, "second").await;
    assert!(!second.start);
    assert_eq!(second.waiting, 1);
}

#[tokio::test]
async fn low_disk_holds_fixed_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // No disk has this much free space, so the host reads low or critical;
    // either holds every start after the first below the fixed limit.
    manager.publish_config(global_config(
        "max_agents = 3\n[overload.warning]\ndisk_free_gib = 1048576",
    ));
    add(&manager, &repo, "first").await;
    assert!(!add(&manager, &repo, "second").await.start);
    // Starting now still starts at once.
    assert!(
        manager
            .queue_add(request(&repo, "urgent", true))
            .await
            .unwrap()
            .start
    );
}

/// An agent connected in `workspace`, which protection may stop, and the
/// receiver that keeps its connection open.
async fn connected_agent(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> (String, tokio::sync::watch::Receiver<bool>) {
    let started = manager
        .begin_execution(
            &workspace.id,
            None,
            crate::daemon::workspace::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .track_agent(
            &started.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            true,
            Duration::from_secs(3600),
        )
        .await;
    (started.plan.id, started.stop)
}

#[tokio::test]
async fn protection_stopped_agents_keep_their_slot_without_load() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = manager
        .create_workspace(&repo.id, "stopped".into(), None, None, None)
        .await
        .unwrap();
    let agent = connected_agent(&manager, &workspace).await;
    let activities = |slots: Vec<Slot>| -> Vec<Activity> {
        slots.into_iter().map(|slot| slot.activity).collect()
    };
    assert!(matches!(
        activities(manager.occupied_slots().await.unwrap())[..],
        [Activity::Running(_)]
    ));
    assert!(
        manager
            .stop_agent_for_overload("critical memory pressure")
            .await
    );
    // Until its processes are proven stopped the agent's load may still show.
    assert!(matches!(
        activities(manager.occupied_slots().await.unwrap())[..],
        [Activity::Running(_)]
    ));
    manager.queue_agent_restore(&agent.0).await.unwrap();
    let _restoring = await_restore(&manager, &agent.0).await;
    assert_eq!(
        activities(manager.occupied_slots().await.unwrap()),
        [Activity::Stopped]
    );
}

/// A hang guard, not a deadline: the awaited step is already admitted.
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("queue step hung")
}

async fn create(manager: &Manager, repository: &Repository, name: &str) -> crate::model::Workspace {
    manager
        .create_workspace(&repository.id, name.into(), None, None, None)
        .await
        .unwrap()
}

/// Agents connected in `workspaces`, each stopped by memory protection with
/// its restore queued in order.
async fn stopped_agents(
    manager: &Manager,
    workspaces: &[&crate::model::Workspace],
) -> Vec<(String, tokio::sync::watch::Receiver<bool>)> {
    let mut agents = Vec::new();
    for workspace in workspaces {
        agents.push(connected_agent(manager, workspace).await);
    }
    for (id, _) in &agents {
        assert!(
            manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        );
        manager.queue_agent_restore(id).await.unwrap();
    }
    agents
}

/// The restore of `execution` waiting for admission, once its waiter counts.
async fn await_restore(
    manager: &std::sync::Arc<Manager>,
    execution: &str,
) -> tokio::task::JoinHandle<Result<u64>> {
    let waiting = {
        let (manager, execution) = (manager.clone(), execution.to_owned());
        tokio::spawn(async move { manager.await_restore_admission(&execution).await })
    };
    bounded(async {
        while !manager
            .restore_waiters
            .lock()
            .unwrap()
            .contains_key(execution)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    waiting
}

fn publish_recovery(manager: &Manager) {
    let epoch = manager
        .recovery_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    manager.recovery_ready.send_replace(Some(epoch));
}

/// Each entry's name, kind and state in queue order.
async fn queue(manager: &Manager) -> Vec<(String, QueueKind, QueueState)> {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name, entry.kind, entry.state))
        .collect()
}

/// Forget a stopped agent as `shoal resume --discard` does once its wrapper ended.
async fn discard(manager: &Manager, workspace: &crate::model::Workspace, execution: &str) {
    manager
        .finish_execution(
            execution.to_owned(),
            crate::daemon::workspace::ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    std::fs::remove_file(crate::execution::recovery::record_path(
        &manager.paths,
        &workspace.id,
        execution,
    ))
    .unwrap();
}

use QueueKind::{Launch, Restore};
use QueueState::{Queued, Starting};

#[tokio::test]
async fn stopped_agents_restore_in_queue_order_before_later_launches() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let (first, second) = (
        create(&manager, &repo, "first").await,
        create(&manager, &repo, "second").await,
    );
    let agents = stopped_agents(&manager, &[&first, &second]).await;
    let launch = add(&manager, &repo, "later").await;
    assert!(!launch.start);
    let launch = launch.entry.unwrap();
    let restoring = [
        await_restore(&manager, &agents[0].0).await,
        await_restore(&manager, &agents[1].0).await,
    ];
    manager.dispatch_queue().await.unwrap();
    assert_eq!(
        queue(&manager).await,
        [
            ("first".into(), Restore, Queued),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );

    // A healthy interval restores the first stopped agent only.
    publish_recovery(&manager);
    manager.dispatch_queue().await.unwrap();
    let [first_restore, second_restore] = restoring;
    bounded(first_restore).await.unwrap().unwrap();
    assert_eq!(manager.published_recovery(), None);
    assert_eq!(
        queue(&manager).await[..2],
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
        ]
    );
    // The restored agent registers and takes over the slot it kept.
    let register =
        |execution: &str| manager.record_execution_child(execution.to_owned(), None, 4242);
    register(&agents[0].0).await.unwrap();
    publish_recovery(&manager);
    manager.dispatch_queue().await.unwrap();
    bounded(second_restore).await.unwrap().unwrap();
    register(&agents[1].0).await.unwrap();
    // Both agents have their slots back, so the launch keeps waiting.
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await, [("later".into(), Launch, Queued)]);
    discard(&manager, &first, &agents[0].0).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&launch.id));
}

#[tokio::test]
async fn a_stop_after_admission_returns_the_restore_to_its_place() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let (first, second) = (
        create(&manager, &repo, "first").await,
        create(&manager, &repo, "second").await,
    );
    let agents = stopped_agents(&manager, &[&first, &second]).await;
    let restoring = await_restore(&manager, &agents[0].0).await;
    publish_recovery(&manager);
    manager.dispatch_queue().await.unwrap();
    let epoch = bounded(restoring).await.unwrap().unwrap();
    // Load worsens again while the restore checks its workspace.
    manager.reset_overload_recovery();
    assert!(
        !manager
            .confirm_restore(&agents[0].0, &first.id, epoch)
            .await
            .unwrap()
    );
    assert_eq!(
        queue(&manager).await,
        [
            ("first".into(), Restore, Queued),
            ("second".into(), Restore, Queued),
        ]
    );
    // The next healthy interval admits it again, and that admission holds.
    let restoring = await_restore(&manager, &agents[0].0).await;
    publish_recovery(&manager);
    manager.dispatch_queue().await.unwrap();
    let epoch = bounded(restoring).await.unwrap().unwrap();
    assert!(
        manager
            .confirm_restore(&agents[0].0, &first.id, epoch)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn admission_consumes_only_the_published_recovery_epoch() {
    let (_temp, manager) = manager().await;
    publish_recovery(&manager);
    let published = manager.published_recovery().unwrap();
    // A stop between the check and the admission withdraws the readings.
    manager.reset_overload_recovery();
    assert_eq!(manager.consume_recovery(published), None);
    publish_recovery(&manager);
    let published = manager.published_recovery().unwrap();
    assert_eq!(manager.consume_recovery(published), Some(published + 1));
    assert_eq!(manager.published_recovery(), None);
}

#[tokio::test]
async fn lowered_limits_hold_restores_until_a_stopped_agent_is_discarded() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let (first, second) = (
        create(&manager, &repo, "first").await,
        create(&manager, &repo, "second").await,
    );
    let agents = stopped_agents(&manager, &[&first, &second]).await;
    let restoring = await_restore(&manager, &agents[0].0).await;
    limit_globally(&manager, 1);
    publish_recovery(&manager);
    // Each restore would take back its own slot, but the other one fills the machine.
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await[0], ("first".into(), Restore, Queued));
    assert!(manager.published_recovery().is_some());

    // `shoal stop` cancels the second restore; its wrapper ends with the
    // recovery record, which still holds the slot until it is discarded.
    manager.remove_restore(&agents[1].0).await;
    manager
        .finish_execution(
            agents[1].0.clone(),
            crate::daemon::workspace::ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await, [("first".into(), Restore, Queued)]);
    std::fs::remove_file(crate::execution::recovery::record_path(
        &manager.paths,
        &second.id,
        &agents[1].0,
    ))
    .unwrap();
    manager.dispatch_queue().await.unwrap();
    bounded(restoring).await.unwrap().unwrap();
    assert_eq!(queue(&manager).await, [("first".into(), Restore, Starting)]);
}

#[tokio::test]
async fn restart_keeps_restores_in_order_without_starting_one_twice() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let (first, second) = (
        create(&manager, &repo, "first").await,
        create(&manager, &repo, "second").await,
    );
    let agents = stopped_agents(&manager, &[&first, &second]).await;
    let launch = add(&manager, &repo, "later").await.entry.unwrap();
    let restoring = await_restore(&manager, &agents[0].0).await;
    publish_recovery(&manager);
    manager.dispatch_queue().await.unwrap();
    bounded(restoring).await.unwrap().unwrap();

    // The daemon restarts before the restored agent registers.
    let restarted = Manager::open(manager.paths.clone()).await.unwrap();
    restarted
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();
    limit_globally(&restarted, 2);
    restarted.refresh_starting_entries().await.unwrap();
    publish_recovery(&restarted);
    // Until the waiting wrappers reattach, nothing restores and the launch
    // behind the restores waits for its slot.
    restarted.dispatch_queue().await.unwrap();
    assert!(!started(&launch.id));
    assert_eq!(
        queue(&restarted).await,
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );
    // The first wrapper never got its restore, so it reattaches waiting and
    // keeps its place instead of queueing a second restore.
    restarted
        .queue_restore(&agents[0].0, "claude", &first.id)
        .await
        .unwrap();
    let restoring = await_restore(&restarted, &agents[0].0).await;
    restarted.dispatch_queue().await.unwrap();
    bounded(restoring).await.unwrap().unwrap();
    assert_eq!(
        queue(&restarted).await,
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );
    // The second wrapper never reattaches; its restore expires and leaves
    // the recovery record to `shoal resume`.
    let past = crate::time::unix_seconds() as i64 - START_GRACE.as_secs() as i64 - 1;
    restarted
        .store
        .run(move |db| {
            db.execute(
                "UPDATE agent_queue SET seen_at=?1 WHERE kind='restore'",
                [past],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    restarted.dispatch_queue().await.unwrap();
    assert_eq!(queue(&restarted).await, [("later".into(), Launch, Queued)]);
    assert!(crate::execution::recovery::pending(&restarted.paths, &second.id).unwrap());
}

#[tokio::test]
async fn dynamic_admission_restores_once_measured_load_has_room() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false",
    ));
    let (busy, stopped) = (
        create(&manager, &repo, "busy").await,
        create(&manager, &repo, "stopped").await,
    );
    let agents = stopped_agents(&manager, &[&stopped]).await;
    // A settled agent keeps running beside the stopped one.
    let _running = connected_agent(&manager, &busy).await;
    let restoring = await_restore(&manager, &agents[0].0).await;
    publish_recovery(&manager);
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let record = |seconds: u64, busy: u64, total: u64| {
        manager.cpu_usage.lock().unwrap().record(
            start + Duration::from_secs(seconds),
            Ok(crate::process::load::CpuTicks { busy, total }),
        );
    };
    // Ninety percent busy leaves no room for the restored agent's load.
    record(0, 0, 0);
    record(5, 90, 100);
    tokio::time::advance(Duration::from_secs(5)).await;
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await[0].2, Queued);
    // A quiet minute makes room.
    record(120, 92, 150);
    record(125, 94, 200);
    tokio::time::advance(Duration::from_secs(120)).await;
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await[0].2, Starting);
    // The paused clock would jump to the hang guard's deadline while the
    // waiter's query runs on the store's thread.
    tokio::time::resume();
    bounded(restoring).await.unwrap().unwrap();
}

#[tokio::test]
async fn launches_in_an_existing_workspace_start_from_the_daemon() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let other = register(&manager, &temp, "other").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    // Without a limit the daemon still starts it rather than the caller.
    let admission = manager
        .queue_add(QueueRequest {
            workspace: Some(child.name.clone()),
            ..request(&repo, "child", false)
        })
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    assert!(!admission.start);
    assert_eq!(entry.workspace_id.as_deref(), Some(child.id.as_str()));
    assert_eq!(entry.state, QueueState::Starting);
    assert!(started(&entry.id));
    // Its worker may attach only the workspace it opens.
    let error = manager
        .queue_attach(&entry.id, &parent.id)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("another workspace"));
    manager.queue_attach(&entry.id, &child.id).await.unwrap();

    let foreign = manager
        .queue_add(QueueRequest {
            workspace: Some(child.name.clone()),
            ..request(&other, "foreign", false)
        })
        .await
        .unwrap_err();
    assert!(format!("{foreign:#}").contains("another repository"));
}

#[tokio::test]
async fn workspace_processes_start_agents_only_below_their_workspace() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            crate::daemon::scope::Caller {
                workspace_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    for (workspace, allowed) in [
        (Some(&child.name), true),
        (Some(&parent.name), false),
        (Some(&sibling.name), false),
        (None, false),
    ] {
        let mut method = crate::protocol::Method::QueueAdd {
            request: QueueRequest {
                workspace: workspace.cloned(),
                ..request(&repo, "launch", false)
            },
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed, "{workspace:?}");
    }
}
