use super::*;
use crate::{
    daemon::{ports::PortRequest, workspace::ExecutionKind},
    test_support::{manager, repository},
};
use futures_util::FutureExt;

#[tokio::test]
async fn agent_exit_hook_reports_disconnects_and_skips_removing_workspaces() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let hook = workspace.path.join("exited");
    fs::write(&hook, "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$SHOAL_AGENT_EXIT_COMPLETE\" >> exits\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(&repo.id, Some("post_agent_exit_cmd = 'exited'\n".into()))
        .await
        .unwrap();
    let cleanup_woken = manager.signals.cleanup_woken();
    manager
        .notify_agent_exit(&workspace, "helper", None, false, None)
        .await;
    manager
        .post_agent_exit(&workspace, "helper", None, false)
        .await;
    assert!(cleanup_woken.now_or_never().is_some());
    assert_eq!(
        fs::read_to_string(workspace.path.join("exits")).unwrap(),
        "helper||false\n"
    );
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    manager
        .notify_agent_exit(&workspace, "helper", Some(143), true, None)
        .await;
    manager
        .post_agent_exit(&workspace, "helper", Some(143), true)
        .await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("exits")).unwrap(),
        "helper||false\n"
    );
    let events = manager.notifications(false, 10).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.kind == NotificationKind::AgentExited)
    );
}

#[tokio::test]
async fn agent_exit_wakes_cleanup_for_completed_workspace() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager.mark_done(&workspace.id, true, false).await.unwrap();
    assert!(manager.signals.cleanup_woken().now_or_never().is_some());
    let complete = manager
        .finish_execution(execution.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(complete);
    manager
        .post_agent_exit(&workspace, "helper", Some(0), complete)
        .await;
    assert!(manager.signals.cleanup_woken().now_or_never().is_some());
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace(&workspace.id).await.is_err());
}

#[tokio::test]
async fn overload_notifications_explain_recovery_policy_and_missing_records() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    for (recover, enabled, expected) in [
        (false, true, "configure [agent_resume]"),
        (true, false, "automatic recovery disabled"),
        (
            true,
            true,
            "automatic session restore waits until system load is healthy",
        ),
    ] {
        let mut config = crate::config::Config::default();
        config.overload.recovery.enabled = enabled;
        manager.publish_config(config);
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        let id = &execution.plan.id;
        manager
            .track_agent(
                id,
                "codex",
                &workspace.id,
                &workspace.name,
                recover,
                Duration::ZERO,
            )
            .await;
        assert!(
            manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        );
        let record = crate::execution::recovery::record_path(&manager.paths, &workspace.id, id);
        assert!(record.is_file());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&record).unwrap()).unwrap(),
            serde_json::json!({
                "agent": "codex",
                "stop_reason": "critical memory pressure",
                "stage": "stopping",
            })
        );
        std::fs::remove_file(&record).unwrap();
        let events = manager.notifications(false, 1).await.unwrap();
        assert!(
            events[0].message.contains(expected),
            "{}",
            events[0].message
        );
        assert!(
            events[0]
                .message
                .contains(&format!("shoal resume {} --execution {id}", workspace.name))
        );
        assert_eq!(
            manager.agent_overload(id).await,
            Some(Protection::load("critical memory pressure"))
        );
        manager
            .notify_agent_exit(
                &workspace,
                "codex",
                Some(143),
                false,
                Some((id, "critical memory pressure")),
            )
            .await;
        let events = manager.notifications(false, 1).await.unwrap();
        assert!(events[0].message.contains("critical memory pressure"));
        assert!(events[0].message.contains("recovery record unavailable"));
        assert!(events[0].message.contains("stays unknown until they exit"));
        manager
            .finish_execution(id.clone(), ExecutionKind::Command, Some(143))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn overload_stops_only_one_agent_and_preserves_work_and_leases() {
    let (_root, manager) = manager().await;
    let repo = repository(_root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    manager
        .acquire_port(&workspace.id, "web".into(), PortRequest::default(), None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
    let command = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let first = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let second = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &second.plan.id,
            "second",
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    // Reattached after a restart, the first agent keeps its earlier launch.
    manager
        .track_agent(
            &first.plan.id,
            "first",
            &workspace.id,
            &workspace.name,
            false,
            Duration::from_secs(3600),
        )
        .await;
    assert!(
        manager
            .stop_agent_for_overload("critical memory pressure")
            .await
    );
    assert!(*second.stop.borrow());
    assert!(!*first.stop.borrow());
    assert!(!*command.stop.borrow());
    let retained = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(retained.workspace.state, workspace.state);
    assert_eq!(retained.ports.len(), 1);
    assert_eq!(retained.executions.len(), 3);
    assert_eq!(
        std::fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::AgentStopped);
    assert!(
        notifications[0]
            .message
            .contains("second: critical memory pressure")
    );
    // Already stopping and disconnected agents cannot be selected again.
    drop(first.stop);
    assert!(
        !manager
            .stop_agent_for_overload("critical memory pressure")
            .await
    );
    manager
        .finish_execution(second.plan.id, ExecutionKind::Command, Some(1))
        .await
        .unwrap();
    assert_eq!(
        manager.executions.with_agents(|agents| agents.len()).await,
        1
    );
}

#[tokio::test]
async fn overload_stops_the_agent_idle_longest_before_working_ones() {
    use crate::{execution::recovery::Record, state::AgentState};
    let (_root, manager) = manager().await;
    let repo = repository(_root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut agents = Vec::new();
    for (name, state, idle_for) in [
        ("working", AgentState::Working, 0),
        ("idle-longest", AgentState::Idle, 120),
        ("idle", AgentState::Idle, 60),
    ] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager
            .track_agent(
                &execution.plan.id,
                name,
                &workspace.id,
                &workspace.name,
                false,
                Duration::ZERO,
            )
            .await;
        manager
            .set_agent_state(
                &workspace.name,
                state,
                Some(execution.plan.id.clone()),
                None,
            )
            .await
            .unwrap();
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE workspace_agent_state SET since=since-?2 WHERE workspace_id=?1",
                    rusqlite::params![id, idle_for],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        agents.push((workspace, execution));
    }
    // The working agent started last, so the newest-first order alone would
    // stop it first.
    let stopped = |index: usize| *agents[index].1.stop.borrow();
    for (index, turn) in [
        (1, AgentState::Idle),
        (2, AgentState::Idle),
        (0, AgentState::Working),
    ] {
        assert!(
            manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        );
        assert!(stopped(index), "agent {index} stops next");
        let (workspace, execution) = &agents[index];
        let path = crate::execution::recovery::record_path(
            &manager.paths,
            &workspace.id,
            &execution.plan.id,
        );
        assert_eq!(Record::load(&path).unwrap().unwrap().turn, Some(turn));
    }
}

#[tokio::test]
async fn a_turn_reported_after_protection_read_the_turns_counts_as_working() {
    use crate::{execution::recovery::Record, state::AgentState};
    let (_root, manager) = manager().await;
    let repo = repository(_root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            "claude",
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    let report = |state| manager.set_agent_state(&workspace.name, state, Some(id.clone()), None);
    report(AgentState::Idle).await.unwrap();
    let turns = manager.agent_turns().await.unwrap();
    assert!(turns[&id].idle());
    // The agent starts a turn while protection scans its processes.
    report(AgentState::Working).await.unwrap();
    let stopped = manager
        .stop_agents_by_turn(
            &Protection::load("critical memory pressure"),
            Select::One,
            turns,
        )
        .await;
    assert_eq!(stopped.len(), 1);
    let path = crate::execution::recovery::record_path(&manager.paths, &workspace.id, &id);
    assert_eq!(
        Record::load(&path).unwrap().unwrap().turn,
        Some(AgentState::Working)
    );
}
