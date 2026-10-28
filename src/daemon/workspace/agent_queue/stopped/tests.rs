use super::*;
use crate::{
    daemon::workspace::agent_queue::tests::{
        connected_agent, create, limit_globally, queue, register, started,
    },
    execution::recovery::{Record, record_path},
    model::Workspace,
    state::QueueKind::Restore,
    state::QueueState::{Queued, Starting},
    test_support::{global_config, manager},
};

/// The variables the restores of these tests start with.
fn herdr() -> BTreeMap<String, String> {
    BTreeMap::from([("HERDR_ENV".into(), "1".into())])
}

/// Save the record of an agent stopped as `execution` in `workspace`.
fn stop(manager: &Manager, workspace: &Workspace, execution: &str, record: &str) {
    let path = record_path(&manager.paths, &workspace.id, execution);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, record).unwrap();
}

const STOPPED: &str = r#"{"agent":"claude","stage":"stopped"}"#;
const SIGNALLED: &str =
    r#"{"agent":"claude","stage":"stopped","restore_request":{"HERDR_ENV":"1"}}"#;

#[tokio::test]
async fn resumed_agents_start_from_the_daemon_as_slots_free() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let mut workspaces = Vec::new();
    for name in ["one", "two", "three"] {
        let workspace = create(&manager, &repo, name).await;
        stop(&manager, &workspace, &format!("{name}-agent"), STOPPED);
        workspaces.push(workspace);
    }
    // Three stopped agents exceed the limit, but a queued restore holds no
    // slot until it is admitted.
    let mut entries = Vec::new();
    for workspace in &workspaces {
        let execution = format!("{}-agent", workspace.name);
        let entry = manager
            .queue_stopped_restore(&workspace.name, &execution, herdr())
            .await
            .unwrap();
        assert_eq!(entry.execution.as_deref(), Some(execution.as_str()));
        entries.push(entry);
    }
    // Queueing again keeps the restore's place.
    let again = manager
        .queue_stopped_restore("one", "one-agent", herdr())
        .await
        .unwrap();
    assert_eq!(again.id, entries[0].id);

    manager.dispatch_queue().await.unwrap();
    assert!(started(&entries[0].id) && started(&entries[1].id));
    assert!(!started(&entries[2].id));
    assert_eq!(
        queue(&manager).await,
        [
            ("one".into(), Restore, Starting),
            ("two".into(), Restore, Starting),
            ("three".into(), Restore, Queued),
        ]
    );

    // The first agent ran and ended, which frees its slot.
    manager
        .consume_queue_entry(&entries[0].id, &workspaces[0].id)
        .await;
    std::fs::remove_file(record_path(&manager.paths, &workspaces[0].id, "one-agent")).unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&entries[2].id));
}

#[tokio::test]
async fn only_an_ended_execution_with_a_record_can_be_queued() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "work").await;
    let missing = manager
        .queue_stopped_restore("work", "absent", herdr())
        .await
        .unwrap_err();
    assert!(format!("{missing:#}").contains("no stopped agent absent"));
    let (running, _connection) = connected_agent(&manager, &workspace).await;
    stop(&manager, &workspace, &running, STOPPED);
    let error = manager
        .queue_stopped_restore("work", &running, herdr())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("has not ended"));
}

#[tokio::test]
async fn signal_stops_queue_their_restore_once_the_execution_has_ended() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let open = create(&manager, &repo, "open").await;
    let done = create(&manager, &repo, "done").await;
    let (running, _connection) = connected_agent(&manager, &open).await;
    stop(&manager, &open, &running, SIGNALLED);
    stop(&manager, &open, "ended", SIGNALLED);
    stop(&manager, &open, "manual", STOPPED);
    manager.mark_done(&done.id, false).await.unwrap();
    stop(&manager, &done, "finished", SIGNALLED);

    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    // Without a limit the daemon starts it at once, as its terminal would.
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].execution.as_deref(), Some("ended"));
    assert!(started(&entries[0].id));
    let request = |workspace: &Workspace, execution: &str| {
        Record::load(&record_path(&manager.paths, &workspace.id, execution))
            .unwrap()
            .unwrap()
            .restore_request
    };
    assert_eq!(request(&open, "ended"), None);
    // The running execution, like a done workspace's agent, keeps its request.
    assert_eq!(request(&open, &running), Some(herdr()));
    assert_eq!(request(&done, "finished"), Some(herdr()));
}

#[tokio::test]
async fn restores_leave_the_queue_once_resolved_or_cancelled() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let busy = create(&manager, &repo, "busy").await;
    let (_running, _connection) = connected_agent(&manager, &busy).await;
    let workspace = create(&manager, &repo, "work").await;
    stop(&manager, &workspace, "resumed", STOPPED);
    stop(&manager, &workspace, "kept", STOPPED);
    for execution in ["resumed", "kept"] {
        manager
            .queue_stopped_restore("work", execution, herdr())
            .await
            .unwrap();
    }
    // Resuming the agent another way consumes its record.
    std::fs::remove_file(record_path(&manager.paths, &workspace.id, "resumed")).unwrap();
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await, [("work".into(), Restore, Queued)]);

    // A stop cancels the restore and leaves the agent stopped.
    manager
        .stop_workspace("work", crate::daemon::workspace::StopRequest::manual())
        .await
        .unwrap();
    assert!(queue(&manager).await.is_empty());
    assert!(record_path(&manager.paths, &workspace.id, "kept").exists());
}

#[tokio::test]
async fn a_restore_whose_agent_never_starts_expires_and_says_so() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "work").await;
    stop(&manager, &workspace, "agent", STOPPED);
    let entry = manager
        .queue_stopped_restore("work", "agent", herdr())
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&entry.id));
    let past = crate::time::unix_seconds() as i64
        - crate::daemon::workspace::agent_queue::START_GRACE.as_secs() as i64
        - 1;
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE agent_queue SET seen_at=?1", [past])?;
            Ok(())
        })
        .await
        .unwrap();

    manager.dispatch_queue().await.unwrap();
    assert!(queue(&manager).await.is_empty());
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(notifications.iter().any(|notification| {
        notification.kind == crate::daemon::notifications::NotificationKind::QueueFailed
            && notification.message
                == "work did not start from the agent queue: its restore ended before the agent started"
    }));
    // The agent stays stopped for `shoal resume`.
    assert!(record_path(&manager.paths, &workspace.id, "agent").exists());
}

#[tokio::test]
async fn a_stop_or_completion_keeps_stopped_agents_down() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let busy = create(&manager, &repo, "busy").await;
    let (_running, _connection) = connected_agent(&manager, &busy).await;
    // A signal stop's request that dispatch has not queued yet.
    let signalled = create(&manager, &repo, "signalled").await;
    stop(&manager, &signalled, "requested", SIGNALLED);
    manager
        .stop_workspace("signalled", crate::daemon::workspace::StopRequest::manual())
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(queue(&manager).await.is_empty());
    let record = Record::load(&record_path(&manager.paths, &signalled.id, "requested"))
        .unwrap()
        .unwrap();
    assert_eq!(record.restore_request, None);

    // A restore waiting when its assignment is done is cancelled.
    let finished = create(&manager, &repo, "finished").await;
    stop(&manager, &finished, "waiting", STOPPED);
    manager
        .queue_stopped_restore("finished", "waiting", herdr())
        .await
        .unwrap();
    manager.mark_done(&finished.id, false).await.unwrap();
    assert!(queue(&manager).await.is_empty());
    assert!(record_path(&manager.paths, &finished.id, "waiting").exists());
}

#[tokio::test]
async fn a_stop_cancels_an_admitted_restore_before_its_agent_registers() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "work").await;
    stop(&manager, &workspace, "agent", STOPPED);
    let entry = manager
        .queue_stopped_restore("work", "agent", herdr())
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(manager.queue_entry_starting(&entry.id).await.unwrap());
    // The agent may take over the slot only while its start is admitted.
    drop(manager.reserve_queue_entry(&entry.id).await.unwrap());
    manager
        .stop_workspace("work", crate::daemon::workspace::StopRequest::manual())
        .await
        .unwrap();
    assert!(queue(&manager).await.is_empty());
    let refused = manager.reserve_queue_entry(&entry.id).await.unwrap_err();
    assert!(format!("{refused:#}").contains("was cancelled"), "{refused:#}");
}

#[tokio::test]
async fn a_request_waits_while_a_resume_holds_its_record() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let workspace = create(&manager, &repo, "work").await;
    stop(&manager, &workspace, "agent", SIGNALLED);
    let path = record_path(&manager.paths, &workspace.id, "agent");
    let claim = crate::execution::recovery::claim(&path).unwrap();
    manager.dispatch_queue().await.unwrap();
    manager
        .cancel_stopped_restores(&workspace.id)
        .await
        .unwrap();
    assert!(queue(&manager).await.is_empty());
    // Neither replaced the record the resume holds.
    assert_eq!(
        Record::load(&path).unwrap().unwrap().restore_request,
        Some(herdr())
    );
    drop(claim);
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await.len(), 1);
}
