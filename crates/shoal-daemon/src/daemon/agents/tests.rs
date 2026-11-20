use super::*;
use crate::daemon::{
    executions::ExecutionKind,
    test_support::{install_test_script, manager, repository},
    workspace_files::Attacher,
};
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use crate::model::PortRequest;
use crate::model::Space;
use crate::model::Workspace;
use futures_util::FutureExt;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn agent_exit_hook_reports_disconnects_and_skips_removing_workspaces() {
    use std::fs;
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
    install_test_script(
        &hook,
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$SHOAL_AGENT_EXIT_COMPLETE\" >> exits\n",
    );
    manager
        .write_local_config(
            &repo.id,
            Some(format!("hooks.post_agent_exit = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    let cleanup_woken = manager.signals.cleanup_woken();
    manager
        .notify_agent_exit(
            &Space::from(workspace.clone()),
            &"helper".into(),
            None,
            false,
            None,
        )
        .await;
    manager
        .post_agent_exit(
            &Space::from(workspace.clone()),
            &"helper".into(),
            None,
            false,
        )
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
        .notify_agent_exit(
            &Space::from(workspace.clone()),
            &"helper".into(),
            Some(143),
            true,
            None,
        )
        .await;
    manager
        .post_agent_exit(
            &Space::from(workspace.clone()),
            &"helper".into(),
            Some(143),
            true,
        )
        .await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("exits")).unwrap(),
        "helper||false\n"
    );
    let events = manager.notifications.list(false, 10).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.kind == AlertKind::AgentExited)
    );
}

#[tokio::test]
async fn a_chats_agent_exit_hook_runs_where_it_works_and_skips_a_removal() {
    use std::fs;
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let exits = root.path().join("exits");
    let hook = root.path().join("exited");
    let script = format!(
        "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$(pwd -P)\" \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$GREETING\" >> '{}'\n",
        exits.display()
    );
    install_test_script(&hook, &script);
    let config = format!(
        "hooks.post_agent_exit = '{}'\n[env]\nGREETING = 'hello'\n",
        hook.display()
    );
    (manager.write_local_config(&repo.id, Some(config)))
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let space = Space::from(chat.clone());
    (manager.post_agent_exit(&space, &"claude".into(), Some(0), true)).await;
    let workdir = fs::canonicalize(chat.workdir.path()).unwrap();
    let ran = format!("{}|claude|0|hello\n", workdir.display());
    assert_eq!(fs::read_to_string(&exits).unwrap(), ran);

    let _removal = manager.space_removals.reserve(&chat.id).unwrap();
    (manager.post_agent_exit(&space, &"claude".into(), Some(0), true)).await;
    assert_eq!(fs::read_to_string(&exits).unwrap(), ran);
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
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(manager.signals.cleanup_woken().now_or_never().is_some());
    let complete = manager
        .finish_execution(execution.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(complete);
    manager
        .post_agent_exit(
            &Space::from(workspace.clone()),
            &"helper".into(),
            Some(0),
            complete,
        )
        .await;
    assert!(manager.signals.cleanup_woken().now_or_never().is_some());
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
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
        (false, true, "resume_command"),
        (true, false, "disabled"),
        (true, true, "healthy"),
    ] {
        let mut config = crate::config::Config::default();
        config.machine.overload.recovery.enabled = enabled;
        manager.publish_config(config);
        let execution = manager
            .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
            .await
            .unwrap();
        let id = &execution.plan.id;
        manager
            .track_agent(
                id,
                &shoal_protocol::executions::TrackedAgent::named("codex"),
                &workspace.id,
                &workspace.name,
                recover,
                Duration::ZERO,
            )
            .await;
        assert!(
            manager
                .stop_for_overload(crate::daemon::overload::Pressure::Memory)
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
                "mode": "worker",
                "for_load": true,
            })
        );
        std::fs::remove_file(&record).unwrap();
        let events = manager.notifications.list(false, 1).await.unwrap();
        assert!(
            events[0].message.contains(expected),
            "{}",
            events[0].message
        );
        assert!(
            events[0]
                .message
                .contains(&format!("shoal resume {} --execution {id}", workspace.id))
        );
        assert_eq!(
            manager.agent_overload(id).await,
            Some(Protection::load("critical memory pressure"))
        );
        manager
            .notify_agent_exit(
                &Space::from(workspace.clone()),
                &"codex".into(),
                Some(143),
                false,
                Some((id, "critical memory pressure")),
            )
            .await;
        let events = manager.notifications.list(false, 1).await.unwrap();
        assert!(events[0].message.contains("critical memory pressure"));
        assert!(events[0].message.contains("recovery record unavailable"));
        manager
            .finish_execution(id.clone(), ExecutionKind::Command, Some(143))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn inspection_and_agent_list_report_an_agent_awaiting_its_restore() {
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
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = &execution.plan.id;
    manager
        .track_agent(
            id,
            &shoal_protocol::executions::TrackedAgent::named("codex"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    let stopped = || async {
        let inspection = manager
            .inspect_workspace(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap();
        let listed = manager.running_agents().await.unwrap();
        (
            inspection.executions[0].stopped.clone(),
            listed[0].stopped.clone(),
        )
    };
    assert_eq!(stopped().await, (None, None));

    assert!(
        manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
            .await
    );
    let protection = Some(Protection::load("critical memory pressure"));
    assert_eq!(stopped().await, (protection.clone(), protection));
    manager
        .finish_execution(id.clone(), ExecutionKind::Command, Some(143))
        .await
        .unwrap();
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
        .leases
        .acquire_port(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "web".into(),
            PortRequest::default(),
            None,
        )
        .await
        .unwrap();
    std::fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
    let command = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let first = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let second = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &second.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("second"),
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
            &shoal_protocol::executions::TrackedAgent::named("first"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::from_secs(3600),
        )
        .await;
    assert!(
        manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
            .await
    );
    assert!(*second.stop.borrow());
    assert!(!*first.stop.borrow());
    assert!(!*command.stop.borrow());
    let retained = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(retained.workspace.state, workspace.state);
    assert_eq!(retained.ports.len(), 1);
    assert_eq!(retained.executions.len(), 3);
    assert_eq!(
        std::fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    let notifications = manager.notifications.list(false, 10).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, AlertKind::AgentStopped);
    assert!(
        notifications[0]
            .message
            .contains("second: critical memory pressure")
    );
    // Already stopping and disconnected agents cannot be selected again.
    drop(first.stop);
    assert!(
        !manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
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
async fn overload_stops_idle_agents_lowest_rank_first_before_working_ones() {
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
            .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager
            .track_agent(
                &execution.plan.id,
                &shoal_protocol::executions::TrackedAgent::named(name),
                &workspace.id,
                &workspace.name,
                false,
                Duration::ZERO,
            )
            .await;
        manager
            .set_agent_state(
                &SpaceSelector::id(&workspace.id),
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
                    "UPDATE space_agent_state SET since=since-?2 WHERE space_id=?1",
                    rusqlite::params![id, idle_for],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        agents.push((workspace, execution));
    }
    // The agent idle longest ranks higher, so the other idle one goes first.
    manager
        .set_priority(
            None,
            &SpaceSelector::id(&agents[1].0.id),
            shoal_schedule::priority::Priority::URGENT,
        )
        .await
        .unwrap();
    // The working agent started last, so the newest-first order alone would
    // stop it first.
    let stopped = |index: usize| *agents[index].1.stop.borrow();
    for (index, turn) in [
        (2, AgentState::Idle),
        (1, AgentState::Idle),
        (0, AgentState::Working),
    ] {
        assert!(
            manager
                .stop_for_overload(crate::daemon::overload::Pressure::Memory)
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
async fn overload_stops_the_agent_the_user_interacted_with_longest_ago_first() {
    use crate::{state::AgentState, time::UnixTime};
    let (_root, manager) = manager().await;
    let repo = repository(_root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut executions = Vec::new();
    let interactions = [
        ("recent", Some(2000)),
        ("earlier", Some(1000)),
        ("working", None),
    ];
    for (name, interacted) in interactions {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        let execution = manager
            .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
            .await
            .unwrap();
        let id = execution.plan.id.clone();
        manager
            .track_agent(
                &id,
                &shoal_protocol::executions::TrackedAgent::named(name),
                &workspace.id,
                &workspace.name,
                false,
                Duration::ZERO,
            )
            .await;
        let state = match name {
            "working" => AgentState::Working,
            _ => AgentState::Idle,
        };
        manager
            .set_agent_state(
                &SpaceSelector::id(&workspace.id),
                state,
                Some(id.clone()),
                None,
            )
            .await
            .unwrap();
        if let Some(at) = interacted {
            let at = UnixTime::from_seconds(at);
            manager.record_interaction(&workspace.id, at).await.unwrap();
        }
        executions.push(execution);
    }
    // Idle, the agents the user interacted with would stop before the
    // working one but for their interactions.
    for index in [2, 1, 0] {
        assert!(
            manager
                .stop_for_overload(crate::daemon::overload::Pressure::Memory)
                .await
        );
        assert!(*executions[index].stop.borrow(), "agent {index} stops next");
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
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    let selected = SpaceSelector::id(&workspace.id);
    let report = |state| manager.set_agent_state(&selected, state, Some(id.clone()), None);
    report(AgentState::Idle).await.unwrap();
    let turns = manager.agent_turns().await.unwrap();
    assert!(turns[&id].idle());
    // The agent starts a turn while protection scans its processes.
    report(AgentState::Working).await.unwrap();
    let stopped = manager
        .stop_agents_by_turn(
            &Protection::load("critical memory pressure"),
            Select::One(crate::daemon::overload::Pressure::Memory),
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

#[tokio::test]
async fn an_agent_records_its_mode_with_its_execution_workspace_and_last_session() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "planned".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(workspace.mode, None);
    let id = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap()
        .plan
        .id;
    let agent = TrackedAgent {
        mode: "plan".into(),
        ..TrackedAgent::named("claude")
    };
    manager
        .track_agent(
            &id,
            &agent,
            &workspace.id,
            &workspace.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    let recorded = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(
        recorded.mode.as_ref().map(|mode| mode.as_str()),
        Some("plan")
    );
    let executions = manager.workspace_executions(&workspace.id).await.unwrap();
    assert_eq!(
        executions[0].mode.as_ref().map(|mode| mode.as_str()),
        Some("plan")
    );
    // The session a restore continues keeps the mode.
    let tracked = manager
        .executions
        .with_agent(&id, |agent| agent.tracked())
        .await
        .unwrap();
    crate::execution::recovery::LastSession::save(&manager.paths, &workspace.id, &tracked, &id)
        .unwrap();
    let path = crate::execution::recovery::LastSession::path(&manager.paths, &workspace.id, &id);
    let saved: crate::execution::recovery::LastSession =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved.mode, "plan");
    // One saved before modes started in the default mode.
    let earlier: crate::execution::recovery::LastSession = serde_json::from_value(
        serde_json::json!({"execution": "e", "agent": "claude", "mode": "worker"}),
    )
    .unwrap();
    assert_eq!(earlier.mode, "worker");
}

/// A running chat in a repository under `root` with a heavy chat mode
/// `pair`, and the execution of its agent, which runs in `mode`.
async fn chat_agent(
    manager: &std::sync::Arc<Manager>,
    root: &std::path::Path,
    mode: &str,
) -> crate::daemon::executions::StartedExecution {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some("[modes.pair]\nkind = 'chat'\n".into())))
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    // Stopped, so the execution registered below is its agent.
    manager.end_executions(&chat.id).await;
    let execution = manager
        .begin_agent_execution(chat.id.as_str())
        .await
        .unwrap();
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: mode.into(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
    };
    (manager.track_agent(
        &execution.plan.id,
        &agent,
        &chat.id,
        &chat.name,
        true,
        Duration::ZERO,
    ))
    .await;
    execution
}

/// Load stops a heavy chat's agent, as it stops a workspace's.
#[tokio::test]
async fn load_stops_a_heavy_chats_agent() {
    let (root, manager) = manager().await;
    let execution = chat_agent(&manager, root.path(), "pair").await;

    let stopped = manager.stop_for_overload(crate::daemon::overload::Pressure::Memory);

    assert!(stopped.await);
    assert!(*execution.stop.borrow());
}

/// Load never stops a light chat's agent, while a lack of disk space stops
/// it with the others, since any agent may be writing.
#[tokio::test]
async fn load_leaves_a_light_chats_agent_running_while_disk_space_stops_it() {
    let (root, manager) = manager().await;
    let execution = chat_agent(&manager, root.path(), "chat").await;

    assert!(
        !manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
            .await
    );
    assert!(!*execution.stop.borrow());
    let protection = Protection::load("low disk space");
    let stopped = manager.stop_agents_for_disk_space(&protection).await;
    assert_eq!(stopped.len(), 1);
    assert!(*execution.stop.borrow());
}

/// Track an agent of `mode` in `workspace`, reporting `state`.
async fn agent_in(
    manager: &Manager,
    workspace: &Workspace,
    mode: &str,
    state: crate::state::AgentState,
) -> crate::daemon::executions::StartedExecution {
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: mode.into(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
    };
    (manager.track_agent(
        &execution.plan.id,
        &agent,
        &workspace.id,
        &workspace.name,
        true,
        Duration::ZERO,
    ))
    .await;
    let selected = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selected, state, Some(execution.plan.id.clone()), None))
        .await
        .unwrap();
    execution
}

/// A coordinator idle in a workspace, never interacted with, and a child
/// workspace below it.
async fn coordinator_and_child(
    manager: &Manager,
    root: &std::path::Path,
) -> (Workspace, Workspace) {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "coordinator".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    (parent, child)
}

/// Load stops a child's working agent before its coordinator's, which
/// would go first on its own, being idle.
#[tokio::test]
async fn load_stops_a_coordinator_only_after_the_agents_below_it() {
    use crate::state::AgentState;
    let (root, manager) = manager().await;
    let (parent, child) = coordinator_and_child(&manager, root.path()).await;
    let coordinator = agent_in(&manager, &parent, "worker", AgentState::Idle).await;
    let worker = agent_in(&manager, &child, "worker", AgentState::Working).await;
    manager
        .record_interaction(&child.id, crate::time::UnixTime::now())
        .await
        .unwrap();

    let memory = crate::daemon::overload::Pressure::Memory;
    assert!(manager.stop_for_overload(memory).await);
    assert!(*worker.stop.borrow());
    assert!(!*coordinator.stop.borrow());
    assert!(manager.stop_for_overload(memory).await);
    assert!(*coordinator.stop.borrow());
}

/// Load never stops a light agent for its own sake, but stopping the heavy
/// coordinator above it stops it first.
#[tokio::test]
async fn load_stops_a_light_agent_below_the_heavy_one_it_stops() {
    use crate::state::AgentState;
    let (root, manager) = manager().await;
    let (parent, child) = coordinator_and_child(&manager, root.path()).await;
    let coordinator = agent_in(&manager, &parent, "worker", AgentState::Working).await;
    let light = agent_in(&manager, &child, "chat", AgentState::Working).await;

    let stopped = (manager.stop_agents_by_turn(
        &Protection::load("critical memory pressure"),
        Select::One(crate::daemon::overload::Pressure::Memory),
        manager.agent_turns().await.unwrap(),
    ))
    .await;
    let order: Vec<_> = stopped.iter().map(|agent| agent.id.clone()).collect();
    assert_eq!(order, [light.plan.id.clone(), coordinator.plan.id.clone()]);
}

/// An agent recorded below a coordinator whose wrapper has yet to
/// reconnect, as after a daemon restart, keeps the coordinator running.
#[tokio::test]
async fn load_leaves_a_coordinator_running_above_an_agent_it_cannot_reach() {
    use crate::state::AgentState;
    let (root, manager) = manager().await;
    let (parent, child) = coordinator_and_child(&manager, root.path()).await;
    let coordinator = agent_in(&manager, &parent, "worker", AgentState::Idle).await;
    let unreached = manager
        .begin_agent_execution(child.id.as_str())
        .await
        .unwrap();
    // Recorded as the daemon that tracked it left it.
    let id = unreached.plan.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE executions SET agent='claude' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();

    let memory = crate::daemon::overload::Pressure::Memory;
    assert!(!manager.stop_for_overload(memory).await);
    assert!(!*coordinator.stop.borrow());
}
