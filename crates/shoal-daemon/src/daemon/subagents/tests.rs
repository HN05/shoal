use super::*;
use crate::{
    daemon::{
        executions::ExecutionKind,
        test_support::{manager, repository},
    },
    model::Workspace,
    state::AgentState,
};

/// A workspace whose tracked agent is idle, and the execution it runs in.
async fn idle_agent(manager: &Manager, root: &std::path::Path) -> (Workspace, ExecutionId) {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .set_agent_state(
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    (workspace, id)
}

/// Report `change` to the subagents of `execution` in `workspace` at `at`.
async fn report(
    manager: &Manager,
    workspace: &Workspace,
    execution: &ExecutionId,
    change: SubagentChange,
    at: UnixTime,
) {
    let space = SpaceSelector::id(&workspace.id);
    (manager.report_subagent(&space, Some(execution.clone()), change, at))
        .await
        .unwrap();
}

/// The turn state protection reads for `execution`.
async fn state(manager: &Manager, execution: &ExecutionId) -> AgentState {
    manager.agent_turns().await.unwrap()[execution].state
}

fn started(agent_id: &str) -> SubagentChange {
    SubagentChange::Started {
        agent_id: agent_id.into(),
    }
}

fn stopped(agent_id: &str) -> SubagentChange {
    SubagentChange::Stopped {
        agent_id: agent_id.into(),
    }
}

#[tokio::test]
async fn an_idle_agent_works_while_one_of_its_subagents_runs() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let now = UnixTime::now();
    for agent_id in ["a1", "a2"] {
        report(&manager, &workspace, &id, started(agent_id), now).await;
    }
    assert_eq!(state(&manager, &id).await, AgentState::Working);

    report(&manager, &workspace, &id, stopped("a1"), now).await;
    assert_eq!(state(&manager, &id).await, AgentState::Working);
    report(&manager, &workspace, &id, stopped("a2"), now).await;
    assert_eq!(state(&manager, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_subagent_whose_stop_is_lost_counts_until_its_requests_stop() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let long_ago = UnixTime::now() - LOST_AFTER - Duration::from_secs(60);
    report(&manager, &workspace, &id, started("a1"), long_ago).await;
    assert_eq!(
        state(&manager, &id).await,
        AgentState::Working,
        "without a proxy, it counts until its stop"
    );

    let space = SpaceSelector::id(&workspace.id);
    (manager.report_subagent_request(&space, id.clone(), long_ago))
        .await
        .unwrap();
    assert_eq!(state(&manager, &id).await, AgentState::Idle);

    (manager.report_subagent_request(&space, id.clone(), UnixTime::now()))
        .await
        .unwrap();
    assert_eq!(state(&manager, &id).await, AgentState::Working);
}

#[tokio::test]
async fn a_session_start_ends_the_subagents_of_the_session_before() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let now = UnixTime::now();
    report(&manager, &workspace, &id, started("a1"), now).await;
    report(
        &manager,
        &workspace,
        &id,
        SubagentChange::SessionStarted,
        now,
    )
    .await;
    assert_eq!(state(&manager, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_stop_that_arrives_before_its_start_leaves_the_subagent_stopped() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let now = UnixTime::now();
    report(&manager, &workspace, &id, stopped("a1"), now).await;
    report(&manager, &workspace, &id, started("a1"), now).await;
    assert_eq!(state(&manager, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_resumed_subagent_runs_again_until_its_next_stop() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let now = UnixTime::now();
    for change in [started("a1"), stopped("a1"), started("a1")] {
        report(&manager, &workspace, &id, change, now).await;
    }
    assert_eq!(state(&manager, &id).await, AgentState::Working);
    report(&manager, &workspace, &id, stopped("a1"), now).await;
    assert_eq!(state(&manager, &id).await, AgentState::Idle);
}

#[tokio::test]
async fn a_session_start_forgets_the_requests_its_proxy_passed_before() {
    let (root, manager) = manager().await;
    let (workspace, id) = idle_agent(&manager, root.path()).await;
    let long_ago = UnixTime::now() - LOST_AFTER - Duration::from_secs(60);
    let space = SpaceSelector::id(&workspace.id);
    (manager.report_subagent_request(&space, id.clone(), long_ago))
        .await
        .unwrap();
    // A restore in place started the session without the proxy.
    report(
        &manager,
        &workspace,
        &id,
        SubagentChange::SessionStarted,
        long_ago,
    )
    .await;
    report(&manager, &workspace, &id, started("a1"), long_ago).await;
    assert_eq!(state(&manager, &id).await, AgentState::Working);
}
