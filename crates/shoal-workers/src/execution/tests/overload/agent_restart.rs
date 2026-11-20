//! `shoal restart` stops an agent and starts the saved session again: in a
//! new wrapper the agent queue starts, or in place in the same terminal.
use super::*;
use crate::{
    model::{QueueEntry, QueueRequest},
    state::QueueState,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// The execution of the workspace's only agent.
async fn agent_execution(manager: &Manager, workspace: &crate::model::Workspace) -> ExecutionId {
    let agents = manager.running_agents().await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected one agent: {agents:?}");
    };
    assert_eq!(agent.space_id, workspace.id);
    assert_eq!(
        agent.agent.as_ref().map(|agent| agent.name()),
        Some("fixture")
    );
    assert!(agent.restorable);
    agent.execution.clone()
}

#[tokio::test]
async fn restart_in_place_restores_the_agent_in_its_wrapper_without_waiting_for_healthy_readings() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    bounded(manager.restart_agent(&execution, true, None))
        .await
        .unwrap();
    // The same wrapper ran the restore command, which exits at once.
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(queue_is_empty(&manager).await);
    assert!(!shoal_core::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn restart_in_place_restores_the_agent_under_the_permissions_given() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    let change = crate::config::permissions::PermissionChange::assigned("push=propose").unwrap();
    let restart = manager.restart_agent(&execution, true, Some(change.clone()));
    bounded(restart).await.unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert!(workspace.path.join("restored").is_file());
    // The space records the restarted agent's narrowing, which its
    // permission checks read.
    let recorded = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(recorded.narrowing, change.applied_to(Default::default()));
    serving.abort();
}

#[tokio::test]
async fn a_restart_in_place_that_cannot_start_the_agent_leaves_it_for_shoal_resume() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    fs::remove_file(workspace.path.join("resume.sh")).unwrap();
    let restarted = bounded(manager.restart_agent(&execution, true, None)).await;
    assert!(restarted.is_err());
    bounded(launched).await.unwrap().unwrap();
    assert!(shoal_core::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let mut saved =
        shoal_core::execution::recovery::Saved::load(&manager.paths, &workspace.id).unwrap();
    saved.retain_unattended();
    assert_eq!(saved.agents.len(), 1, "{saved:?}");
    serving.abort();
}

#[tokio::test]
async fn a_restart_resumes_only_in_a_ready_recorded_workspace() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let restarted = bounded(manager.restart_agent(&execution, false, None)).await;
    assert!(restarted.is_err());
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(shoal_core::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

/// A launch in a new workspace of the fixture's repository, which waits for
/// a slot.
async fn waiting_launch(manager: &Manager, workspace: &crate::model::Workspace) -> QueueRequest {
    let repository = workspace.repository().clone();
    let waiting = (manager.create_workspace(repository, "waiting".into(), None, None, None))
        .await
        .unwrap();
    QueueRequest {
        source: None,
        issue_labels: Vec::new(),
        narrowing: Default::default(),
        name: waiting.name.clone(),
        agent: "claude".into(),
        mode: None,
        issue_title: None,
        plan: "{}".into(),
        env: Default::default(),
        now: false,
        space: SpaceSelector::id(&waiting.id),
        starts_in_caller: false,
        blockers: Vec::new(),
    }
}

/// The messages of the agent exits the user was told about.
async fn exit_notifications(manager: &Manager) -> Vec<String> {
    manager
        .notifications
        .list(false, 10)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == AlertKind::AgentExited)
        .map(|event| event.message)
        .collect()
}

/// The old wrapper cannot start the agent, as one whose process tree lost
/// access to the workspace cannot; the new wrapper the agent queue starts
/// does, in the slot the agent kept, while a launch waits for one.
#[tokio::test]
async fn restart_starts_the_agent_in_a_new_wrapper_in_the_slot_it_kept() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false",
    ));
    let waiting = waiting_launch(&manager, &workspace).await;
    let waiting = manager.queue_add(waiting).await.unwrap();
    let waiting = waiting.entry.unwrap();
    assert_eq!(waiting.state, QueueState::Queued);
    fs::remove_file(workspace.path.join("resume.sh")).unwrap();
    let restarted = tokio::spawn({
        let (manager, execution) = (manager.clone(), execution.clone());
        async move { manager.restart_agent(&execution, false, None).await }
    });
    // The old wrapper finishes with the agent's record. Dispatch, which
    // ends restarts whose start left the queue, waits while the restore is
    // recorded.
    let gate = manager.agent_queue.lock_admission().await;
    bounded(launched).await.unwrap().unwrap();
    manager.fail_lost_renewals().await.unwrap();
    drop(gate);
    let restore = starting_restore(&manager, execution.as_str()).await;
    assert_eq!(restore.state, QueueState::Starting);
    bounded(async {
        while !shoal_daemon::test_daemon::STARTED_QUEUED
            .lock()
            .unwrap()
            .iter()
            .any(|(started, _)| *started == restore.id)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(shoal_core::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    // No wrapper runs the agent now, and its slot stays the restore's.
    manager.dispatch_queue().await.unwrap();
    let state = |entries: &[QueueEntry], id: &str| {
        entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.state)
    };
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(
        state(&entries, waiting.id.as_str()),
        Some(QueueState::Queued)
    );
    assert!(!restarted.is_finished());
    // The new wrapper takes over the restore's slot and starts the agent.
    let mut renewed = child_command(
        &manager,
        &workspace,
        "execution::tests::overload::agent_restart::renewed_agent_child",
    );
    renewed.env("SHOAL_TEST_QUEUE_ENTRY", &restore.id);
    let renewed = run_child(renewed);
    bounded(restarted).await.unwrap().unwrap();
    wait_launched(&manager, &workspace, "renewed", 1).await;
    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(state(&entries, restore.id.as_str()), None);
    assert_eq!(
        state(&entries, waiting.id.as_str()),
        Some(QueueState::Queued)
    );
    assert_eq!(exit_notifications(&manager).await, Vec::<String>::new());
    let agents = manager.running_agents().await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected the restarted agent: {agents:?}");
    };
    assert_ne!(agent.execution, execution);
    bounded(manager.stop_space(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(renewed).await.unwrap().unwrap();
    serving.abort();
}

/// A new wrapper that cannot start answers the restart with why, and leaves
/// the agent's record for `shoal resume`.
#[tokio::test]
async fn a_restart_whose_new_wrapper_fails_to_start_leaves_the_agent_for_shoal_resume() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    let restarted = tokio::spawn({
        let (manager, execution) = (manager.clone(), execution.clone());
        async move { manager.restart_agent(&execution, false, None).await }
    });
    bounded(launched).await.unwrap().unwrap();
    let restore = starting_restore(&manager, execution.as_str()).await;
    manager
        .fail_queue_entry(&restore.id, "no terminal host can start queued agents")
        .await
        .unwrap();
    let error = bounded(restarted).await.unwrap().unwrap_err();
    assert_eq!(
        format!("{error}"),
        "no terminal host can start queued agents"
    );
    assert!(shoal_core::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(manager.running_agents().await.unwrap().is_empty());
    serving.abort();
}

/// A request for the agent while no wrapper runs it finds it by the
/// execution it ran as, and since the restart settled how it starts the
/// agent, it cannot join it; the agent the new wrapper runs is listed as
/// having run as it.
#[tokio::test]
async fn a_restart_in_a_new_wrapper_follows_the_agent_from_the_execution_it_ran_as() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    let restarted = tokio::spawn({
        let (manager, execution) = (manager.clone(), execution.clone());
        async move { manager.restart_agent(&execution, false, None).await }
    });
    bounded(launched).await.unwrap().unwrap();
    let restore = starting_restore(&manager, execution.as_str()).await;
    let agents = manager.running_agents().await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected the restarting agent: {agents:?}");
    };
    assert_eq!(agent.execution, execution);
    let later = manager.restart_agent(&execution, false, None);
    bounded(later).await.unwrap_err();
    let mut renewed = child_command(
        &manager,
        &workspace,
        "execution::tests::overload::agent_restart::renewed_agent_child",
    );
    renewed.env("SHOAL_TEST_QUEUE_ENTRY", &restore.id);
    let renewed = run_child(renewed);
    bounded(restarted).await.unwrap().unwrap();
    let agents = manager.running_agents().await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected the restarted agent: {agents:?}");
    };
    assert_eq!(agent.restarted_from, std::slice::from_ref(&execution));
    bounded(manager.stop_space(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(renewed).await.unwrap().unwrap();
    serving.abort();
}

/// A restart's new wrapper restores the agent from its record, as
/// `shoal resume` does, but the restart keeps a done workspace's completion.
#[tokio::test]
async fn a_restart_in_a_new_wrapper_keeps_a_done_workspaces_completion() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    manager
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            shoal_protocol::assignments::Done::default(),
            &shoal_protocol::files::Attacher::User,
        )
        .await
        .unwrap();
    let restarted = tokio::spawn({
        let (manager, execution) = (manager.clone(), execution.clone());
        async move { manager.restart_agent(&execution, false, None).await }
    });
    bounded(launched).await.unwrap().unwrap();
    let restore = starting_restore(&manager, execution.as_str()).await;
    let record =
        shoal_core::execution::recovery::record_path(&manager.paths, &workspace.id, &execution);
    let mut renewed = child_command(
        &manager,
        &workspace,
        "execution::tests::overload::resumed_agent_child",
    );
    renewed
        .env("SHOAL_TEST_RECORD", &record)
        .env("SHOAL_TEST_QUEUE_ENTRY", &restore.id);
    let renewed = run_child(renewed);
    bounded(restarted).await.unwrap().unwrap();
    wait_start_handled(&manager, &workspace).await;
    assert!(manager.completion(&workspace.id).await.unwrap().is_some());
    bounded(manager.stop_space(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(renewed).await.unwrap().unwrap();
    serving.abort();
}

/// The new wrapper the agent queue starts for a restart: it takes over the
/// queue entry `SHOAL_TEST_QUEUE_ENTRY` names, and its agent writes `renewed`.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by restart tests"]
async fn renewed_agent_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    crate::env::launch_queue_entry(&std::env::var("SHOAL_TEST_QUEUE_ENTRY").unwrap().into());
    run_tracked_agent("trap 'exit 0' TERM; touch renewed; while :; do sleep 1; done").await;
}
