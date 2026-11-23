use std::{sync::Arc, time::Duration};

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::restarts::record::{self, Recorded};
use crate::{
    config::permissions::PermissionChange,
    daemon::{
        agents::ProtectionExt as _,
        executions::ExecutionKind,
        manager::Manager,
        scope::Caller,
        test_support::{manager, repository},
    },
    model::{AgentRestart, RestartOutcome, Workspace},
    protocol::Reattach,
    state::AgentState,
};

async fn workspace(root: &std::path::Path, manager: &Manager) -> Workspace {
    let repo = repository(root, "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agents".into(), None, None, None)
        .await
        .unwrap()
}

async fn track(
    manager: &Manager,
    workspace: &Workspace,
    id: &ExecutionId,
    name: &str,
    recover: bool,
) {
    manager
        .agents
        .track_agent(
            manager,
            id,
            &shoal_protocol::executions::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            recover,
            Duration::ZERO,
        )
        .await;
}

/// A workspace whose `claude` agent runs as the returned execution and
/// works on a turn.
async fn working(
    root: &std::path::Path,
    manager: &Manager,
) -> (Workspace, crate::daemon::executions::StartedExecution) {
    let workspace = workspace(root, manager).await;
    let execution = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    track(manager, &workspace, &execution.plan.id, "claude", true).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(manager, &space, AgentState::Working, None, None))
    .await
    .unwrap();
    (workspace, execution)
}

/// The restarts the daemon recorded.
async fn recorded(manager: &Manager) -> Vec<Recorded> {
    manager.store.read(|db| record::all(db)).await.unwrap()
}

fn waiting() -> AgentRestart {
    AgentRestart {
        wait: true,
        in_place: true,
        ..Default::default()
    }
}

/// Record a restart of the agent of `workspace`, running as `execution`,
/// for `caller` once the turn ends, then fail it as the agent's wrapper
/// would, with no request left to answer.
async fn fail_after_the_turn(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    execution: &crate::daemon::executions::StartedExecution,
    caller: Option<&Caller>,
) {
    let space = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .restart_space(&**manager, caller, &space, waiting()))
    .await
    .unwrap();
    (manager
        .agents
        .set_agent_state(&**manager, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    let acting = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move { manager.agents.act_on_restart(&*manager, &id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = &execution.plan.id;
    manager.agents.fail_restart(id, FAILURE.into()).await;
    assert!(!acting.await.unwrap());
}

const FAILURE: &str = "automatic restore failed";

#[tokio::test]
async fn a_deferred_restart_that_fails_reaches_the_coordinator_that_asked() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let repository = workspace.repository().clone();
    let coordinator = manager
        .create_workspace(&repository, "coordinator".into(), None, None, None)
        .await
        .unwrap();
    let caller = Caller {
        space_id: coordinator.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    fail_after_the_turn(&manager, &workspace, &execution, Some(&caller)).await;
    let coordinator = SpaceSelector::id(&coordinator.id);
    let messages = manager
        .agents
        .agent_messages(&*manager, &coordinator)
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.message.contains(FAILURE)),
        "{messages:?}"
    );
}

#[tokio::test]
async fn a_recorded_restart_goes_with_its_agent_rather_than_restart_the_next() {
    let (root, manager) = manager().await;
    let (workspace, first) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .restart_space(&*manager, None, &space, waiting()))
    .await
    .unwrap();
    let ended = first.plan.id.clone();
    (manager
        .agents
        .finish_execution(&*manager, ended, ExecutionKind::Command, Some(0)))
    .await
    .unwrap();
    let next = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    track(&manager, &workspace, &next.plan.id, "claude", true).await;
    (manager
        .agents
        .set_agent_state(&*manager, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    assert!(
        !manager
            .agents
            .act_on_restart(&*manager, &workspace.id)
            .await
    );
    assert!(!*next.stop.borrow());
    assert!(recorded(&manager).await.is_empty());
}

/// A workspace whose `claude` agent runs as a wrapper of this process and
/// works, with the reattachment its wrapper asks for after a takeover once
/// a restart stopped the agent, and what its connection receives its stop
/// on.
async fn wrapped_agent(
    root: &std::path::Path,
    manager: &Manager,
) -> (Workspace, Reattach, tokio::sync::watch::Receiver<bool>) {
    let workspace = workspace(root, manager).await;
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let started = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    (manager
        .agents
        .record_execution_child(id.clone(), Some(wrapper.clone()), wrapper.pid))
    .await
    .unwrap();
    track(manager, &workspace, &id, "claude", true).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(manager, &space, AgentState::Working, None, None))
    .await
    .unwrap();
    let request = Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        execution: id,
        wrapper: wrapper.clone(),
        child: Some(wrapper.clone()),
        group_id: wrapper.pid,
        scope_token: started.plan.scope_token.clone(),
        agent: Some("claude".into()),
        recover: true,
        running_ms: 0,
        awaiting_restore: Some(crate::model::Protection::restart()),
        between_runs: false,
        reports_status: false,
    };
    (workspace, request, started.stop)
}

/// Have the restart that stops the agent on `stop`, which runs as `id`,
/// wait for the stopped agent's processes.
async fn wait_for_processes(
    manager: &Manager,
    id: &ExecutionId,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager
        .agents
        .notify_restore_waits(manager, id, "PID 7 survives")
        .await;
}

/// A workspace whose `claude` agent runs as a wrapper of this process,
/// with the reattachment its wrapper asks for after a takeover, once the
/// daemon began `restart`, recorded until the agent's turn ended, and it
/// waits for the stopped agent's processes.
async fn waiting_for_processes(
    root: &std::path::Path,
    manager: &Arc<Manager>,
    restart: AgentRestart,
) -> (Workspace, Reattach) {
    let (workspace, request, stop) = wrapped_agent(root, manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let restart = AgentRestart {
        wait: true,
        ..restart
    };
    (manager
        .agents
        .restart_space(&**manager, None, &space, restart))
    .await
    .unwrap();
    (manager
        .agents
        .set_agent_state(&**manager, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    let acting = tokio::spawn({
        let (manager, space) = (manager.clone(), workspace.id.clone());
        async move { manager.agents.act_on_restart(&*manager, &space).await }
    });
    wait_for_processes(manager, &request.execution, stop).await;
    assert!(!acting.await.unwrap());
    (workspace, request)
}

/// Reattach the wrapper of `request` as to the daemon that took over,
/// which knows the restart under way only from its record, returning what
/// the wrapper's connection receives its stop and resume on.
async fn take_over(manager: &Manager, request: &Reattach) -> tokio::sync::watch::Receiver<bool> {
    manager.agents.detach_execution(&request.execution).await;
    (manager.spaces.interrupted_operations()).await.unwrap();
    manager
        .agents
        .reattach_execution(manager, request)
        .await
        .unwrap()
        .stop
}

#[tokio::test]
async fn a_restart_waiting_for_processes_keeps_its_options_through_takeovers() {
    let (root, manager) = manager().await;
    let change = PermissionChange::assigned("push=direct").unwrap();
    let restart = AgentRestart {
        in_place: true,
        change: Some(change.clone()),
        ..Default::default()
    };
    let (workspace, request) = waiting_for_processes(root.path(), &manager, restart).await;
    // It is under way, not kept for later.
    let space = SpaceSelector::id(&workspace.id);
    assert_eq!(
        manager
            .agents
            .cancel_restart(&*manager, &space)
            .await
            .unwrap(),
        None
    );
    let id = &request.execution;
    let mut wrapper = None;
    for _ in 0..2 {
        wrapper = Some(take_over(&manager, &request).await);
        assert_eq!(
            manager.agents.restart_change(id).await,
            Some(change.clone())
        );
        assert!(recorded(&manager).await[0].begun);
    }
    assert!(manager.agents.settle_restart(id).await);
    assert!(
        manager
            .agents
            .resume_agent_execution(&*manager, id, None)
            .await
            .unwrap()
    );
    assert!(!*wrapper.unwrap().borrow());
    assert!(recorded(&manager).await.is_empty());
}

#[tokio::test]
async fn a_restart_begun_at_once_keeps_its_options_through_a_takeover() {
    let (root, manager) = manager().await;
    let (workspace, request, stop) = wrapped_agent(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let change = PermissionChange::assigned("push=direct").unwrap();
    let restart = AgentRestart {
        in_place: true,
        change: Some(change.clone()),
        ..Default::default()
    };
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), space.clone());
        async move {
            manager
                .agents
                .restart_space(&*manager, None, &space, restart)
                .await
        }
    });
    wait_for_processes(&manager, &request.execution, stop).await;
    let outcome = restarting.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Waits { .. }),
        "{outcome:?}"
    );

    take_over(&manager, &request).await;
    let id = &request.execution;
    assert_eq!(manager.agents.restart_change(id).await, Some(change));
    assert!(manager.agents.settle_restart(id).await);
}

#[tokio::test]
async fn a_restart_under_way_keeps_the_options_of_a_request_it_took_through_a_takeover() {
    let (root, manager) = manager().await;
    let earlier = PermissionChange::assigned("push=direct").unwrap();
    let restart = AgentRestart {
        change: Some(earlier.clone()),
        ..Default::default()
    };
    let (workspace, request) = waiting_for_processes(root.path(), &manager, restart).await;
    let newer = PermissionChange::assigned("merge=direct").unwrap();
    let restart = AgentRestart {
        in_place: true,
        change: Some(newer.clone()),
        ..Default::default()
    };
    let space = SpaceSelector::id(&workspace.id);
    let outcome = manager
        .agents
        .restart_space(&*manager, None, &space, restart)
        .await
        .unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Waits { .. }),
        "{outcome:?}"
    );

    take_over(&manager, &request).await;
    let id = &request.execution;
    let merged = PermissionChange::then(Some(earlier), Some(newer));
    assert_eq!(manager.agents.restart_change(id).await, merged);
    assert!(manager.agents.settle_restart(id).await);
}

#[tokio::test]
async fn a_wrapper_that_reattaches_without_the_restarts_stop_lets_it_begin_again() {
    let (root, manager) = manager().await;
    let (_workspace, request) =
        waiting_for_processes(root.path(), &manager, AgentRestart::default()).await;
    let running = Reattach {
        awaiting_restore: None,
        ..request
    };
    take_over(&manager, &running).await;
    assert!(!recorded(&manager).await[0].begun);
}
