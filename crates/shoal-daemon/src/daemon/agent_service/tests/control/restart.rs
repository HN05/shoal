use shoal_ids::execution_ids::ExecutionId;
use std::time::Duration;

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        test_support::{manager, repository},
    },
    model::Workspace,
    state::AgentState,
};
use shoal_protocol::space_names::SpaceSelector;

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

#[tokio::test]
async fn a_shared_workspace_keeps_the_turn_of_the_agent_that_reported_last() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let mut ids = Vec::new();
    for name in ["claude", "codex"] {
        let execution = manager
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
        track(&manager, &workspace, &execution.plan.id, name, true).await;
        ids.push(execution.plan.id);
    }
    // The workspace keeps only the later report, which leaves the other
    // agent's turn unknown, as one that never reported.
    for (id, state) in ids.iter().zip([AgentState::Idle, AgentState::Working]) {
        manager
            .agents
            .set_agent_state(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                state,
                Some(id.clone()),
                None,
            )
            .await
            .unwrap();
    }
    let states: std::collections::HashMap<_, _> = manager
        .agents
        .running_agents()
        .await
        .unwrap()
        .into_iter()
        .map(|agent| (agent.execution, agent.state))
        .collect();
    assert_eq!(states[&ids[0]], None);
    assert_eq!(states[&ids[1]], Some(AgentState::Working));
    let status = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(
        status.workspace.agent_state.map(|status| status.state),
        Some(AgentState::Working)
    );
}
