use super::*;
use crate::{
    daemon::workspace::agent_queue::tests::{
        connected_agent, create, limit_globally, queue, register, started,
    },
    daemon::workspace_files::Attacher,
    execution::recovery::record_path,
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

/// Save the record and restore request a signal stop leaves.
fn signal_stop(manager: &Manager, workspace: &Workspace, execution: &str) {
    stop(manager, workspace, execution, STOPPED);
    let request = record_path(&manager.paths, &workspace.id, execution)
        .with_file_name(format!("{execution}.restore-request.json"));
    std::fs::write(request, r#"{"HERDR_ENV":"1"}"#).unwrap();
}

/// The variables the restore requested for `execution` starts with.
fn requested(
    manager: &Manager,
    workspace: &Workspace,
    execution: &str,
) -> Option<BTreeMap<String, String>> {
    recovery::restore_requests(&manager.paths, &workspace.id)
        .unwrap()
        .into_iter()
        .find_map(|(id, env)| (id == execution).then_some(env))
}

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
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let open = create(&manager, &repo, "open").await;
    let done = create(&manager, &repo, "done").await;
    let (running, _connection) = connected_agent(&manager, &open).await;
    signal_stop(&manager, &open, &running);
    signal_stop(&manager, &open, "ended");
    stop(&manager, &open, "manual", STOPPED);
    manager
        .mark_done(&done.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    signal_stop(&manager, &done, "finished");

    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    // Without a limit the daemon starts it at once, as its terminal would.
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].execution.as_deref(), Some("ended"));
    assert!(started(&entries[0].id));
    assert_eq!(requested(&manager, &open, "ended"), None);
    // The running execution, like a done workspace's agent, keeps its request.
    assert_eq!(requested(&manager, &open, &running), Some(herdr()));
    assert_eq!(requested(&manager, &done, "finished"), Some(herdr()));
}

/// The record of an agent stopped between turns.
const IDLE: &str = r#"{"agent":"claude","stage":"stopped","turn":"idle"}"#;

/// The queued entry restoring `execution`.
async fn entry(manager: &Manager, execution: &str) -> QueueEntry {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.execution.as_deref() == Some(execution))
        .unwrap()
}

#[tokio::test]
async fn a_signal_stopped_idle_agent_waits_for_an_update_unless_resumed() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let mut workspaces = Vec::new();
    for (name, record) in [("idle", IDLE), ("resumed", IDLE), ("working", STOPPED)] {
        let workspace = create(&manager, &repo, name).await;
        let execution = format!("{name}-agent");
        signal_stop(&manager, &workspace, &execution);
        stop(&manager, &workspace, &execution, record);
        workspaces.push(workspace);
    }
    // The idle agents hold no slot, so the one stopped mid-turn takes it.
    manager.dispatch_queue().await.unwrap();
    let working = entry(&manager, "working-agent").await;
    assert!(started(&working.id) && !working.waits_for_update);
    for execution in ["idle-agent", "resumed-agent"] {
        let waiting = entry(&manager, execution).await;
        assert!(waiting.waits_for_update && waiting.state == Queued);
    }

    // Resuming one keeps its place, and it waits only for a slot.
    manager
        .queue_stopped_restore("resumed", "resumed-agent", herdr())
        .await
        .unwrap();
    assert!(!entry(&manager, "resumed-agent").await.waits_for_update);
    manager
        .consume_queue_entry(&working.id, &workspaces[2].id)
        .await;
    std::fs::remove_file(record_path(
        &manager.paths,
        &workspaces[2].id,
        "working-agent",
    ))
    .unwrap();
    manager.dispatch_queue().await.unwrap();
    let resumed = entry(&manager, "resumed-agent").await;
    assert!(started(&resumed.id));
    assert_eq!(entry(&manager, "idle-agent").await.state, Queued);

    // An update for the idle agent restores it once a slot is free.
    manager
        .consume_queue_entry(&resumed.id, &workspaces[1].id)
        .await;
    std::fs::remove_file(record_path(
        &manager.paths,
        &workspaces[1].id,
        "resumed-agent",
    ))
    .unwrap();
    manager
        .send_agent_message(
            "idle",
            "Review requested".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&entry(&manager, "idle-agent").await.id));
}

#[tokio::test]
async fn a_queued_restore_of_an_idle_agent_keeps_its_workspace_from_no_cleanup() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let idle = create(&manager, &repo, "idle").await;
    signal_stop(&manager, &idle, "idle-agent");
    stop(&manager, &idle, "idle-agent", IDLE);
    let working = create(&manager, &repo, "working").await;
    signal_stop(&manager, &working, "working-agent");
    manager.dispatch_queue().await.unwrap();
    assert!(entry(&manager, "idle-agent").await.waits_for_update);
    assert!(manager.cleanup_candidacy(&idle.id).await.unwrap().is_ok());
    assert_eq!(
        manager.cleanup_candidacy(&working.id).await.unwrap(),
        Err(crate::model::CleanupReason::Recovering)
    );

    // An update gives the agent work, though no reading has found free disk
    // space for the new configuration, which holds its restore back.
    manager.publish_config(global_config(""));
    manager
        .send_agent_message(
            "idle",
            "Review requested".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert_eq!(entry(&manager, "idle-agent").await.state, Queued);
    assert_eq!(
        manager.cleanup_candidacy(&idle.id).await.unwrap(),
        Err(crate::model::CleanupReason::Recovering)
    );
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
    signal_stop(&manager, &signalled, "requested");
    manager
        .stop_workspace("signalled", crate::daemon::workspace::StopRequest::manual())
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(queue(&manager).await.is_empty());
    assert_eq!(requested(&manager, &signalled, "requested"), None);
    assert!(record_path(&manager.paths, &signalled.id, "requested").exists());

    // A restore waiting when its assignment is done is cancelled.
    let finished = create(&manager, &repo, "finished").await;
    stop(&manager, &finished, "waiting", STOPPED);
    manager
        .queue_stopped_restore("finished", "waiting", herdr())
        .await
        .unwrap();
    manager
        .mark_done(&finished.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    assert!(queue(&manager).await.is_empty());
    assert!(record_path(&manager.paths, &finished.id, "waiting").exists());
}

#[tokio::test]
async fn a_manual_stop_keeps_stopped_agents_as_last_sessions() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "work").await;
    signal_stop(&manager, &workspace, "signalled");
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await.len(), 1);
    let parent = create(&manager, &repo, "parent").await;
    manager
        .stop_workspace_manually("work", false, Some(&parent.id))
        .await
        .unwrap();
    // Nothing restores it, and it holds no slot or cleanup back.
    assert!(queue(&manager).await.is_empty());
    assert!(!record_path(&manager.paths, &workspace.id, "signalled").exists());
    assert!(!recovery::pending(&manager.paths, &workspace.id).unwrap());
    let [last] = recovery::LastSession::all(&manager.paths, &workspace.id)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        (last.execution.as_str(), last.agent.as_str()),
        ("signalled", "claude")
    );
    // A signal stopped it before this stop, so its workspace process cannot
    // resume it.
    assert_eq!(last.stopped_by, None);
}

#[tokio::test]
async fn a_stop_claims_only_the_agents_nothing_stopped_before() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let workspace = create(&manager, &repo, "work").await;
    let (running, _running) = connected_agent(&manager, &workspace).await;
    // A protection stop's agent keeps its execution while its wrapper waits.
    let (paused, _paused) = connected_agent(&manager, &workspace).await;
    stop(&manager, &workspace, &paused, STOPPED);
    let unstopped = manager.unstopped_executions(&workspace.id).await.unwrap();
    assert_eq!(unstopped, [running]);
}

#[tokio::test]
async fn a_last_session_queues_and_takes_a_slot_once_admitted() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let mut entries = Vec::new();
    for name in ["one", "two"] {
        let workspace = create(&manager, &repo, name).await;
        let execution = format!("{name}-agent");
        recovery::LastSession {
            execution: execution.clone(),
            agent: "claude".into(),
            session: None,
            moved: false,
            origin: None,
            stopped_by: None,
        }
        .write(&manager.paths, &workspace.id)
        .unwrap();
        let entry = manager
            .queue_stopped_restore(name, &execution, herdr())
            .await
            .unwrap();
        entries.push(entry);
    }
    // The admitted restore counts as a launch until its agent registers.
    manager.dispatch_queue().await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&entries[0].id));
    assert!(!started(&entries[1].id));
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
    assert!(
        format!("{refused:#}").contains("was cancelled"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn a_shutdown_returns_an_admitted_restore_to_the_queue_when_its_agent_registers() {
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
    // The shutdown begins after admission, before the agent registers.
    manager.agent_queue.suspend(true).await;
    let refused = manager.reserve_queue_entry(&entry.id).await.unwrap_err();
    assert!(
        format!("{refused:#}").contains("the system is shutting down"),
        "{refused:#}"
    );
    manager.dispatch_queue().await.unwrap();
    assert_eq!(queue(&manager).await[0].2, Queued);
    // Once the queue admits again, the restore starts again.
    manager.agent_queue.suspend(false).await;
    manager.dispatch_queue().await.unwrap();
    drop(manager.reserve_queue_entry(&entry.id).await.unwrap());
}

#[tokio::test]
async fn a_stop_forgets_a_request_while_a_resume_holds_its_record() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let workspace = create(&manager, &repo, "work").await;
    signal_stop(&manager, &workspace, "agent");
    let path = record_path(&manager.paths, &workspace.id, "agent");
    let resuming = crate::execution::recovery::claim(&path).unwrap();
    manager
        .stop_workspace("work", crate::daemon::workspace::StopRequest::manual())
        .await
        .unwrap();
    // The resume fails and keeps the record, yet nothing restores the agent.
    drop(resuming);
    manager.dispatch_queue().await.unwrap();
    assert!(queue(&manager).await.is_empty());
    assert!(path.exists());
}

#[tokio::test]
async fn a_request_whose_record_is_gone_is_forgotten() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let workspace = create(&manager, &repo, "work").await;
    signal_stop(&manager, &workspace, "agent");
    // A resume consumed the record before dispatch saw the request.
    std::fs::remove_file(record_path(&manager.paths, &workspace.id, "agent")).unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(queue(&manager).await.is_empty());
    assert_eq!(requested(&manager, &workspace, "agent"), None);
}

#[tokio::test]
async fn stop_all_selects_running_agents_and_pending_restores() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let running = create(&manager, &repo, "running").await;
    let (_execution, _connection) = connected_agent(&manager, &running).await;
    let queued = create(&manager, &repo, "queued").await;
    stop(&manager, &queued, "waiting", STOPPED);
    manager
        .queue_stopped_restore("queued", "waiting", herdr())
        .await
        .unwrap();
    let requested = create(&manager, &repo, "requested").await;
    signal_stop(&manager, &requested, "agent");
    let idle = create(&manager, &repo, "idle").await;
    stop(&manager, &idle, "manual", STOPPED);

    let mut targets: Vec<String> = manager
        .stop_targets()
        .await
        .unwrap()
        .into_iter()
        .map(|workspace| workspace.name)
        .collect();
    targets.sort();
    assert_eq!(targets, ["queued", "requested", "running"]);
}
