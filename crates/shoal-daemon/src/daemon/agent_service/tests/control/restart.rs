use crate::model::Space;
use shoal_ids::execution_ids::ExecutionId;
use std::time::Duration;

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        test_support::{manager, repository},
    },
    execution::recovery,
    model::Workspace,
    sessions::Origin,
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
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(
        status.workspace.agent_state.map(|status| status.state),
        Some(AgentState::Working)
    );
}

/// Hand the restart of the agent `id` runs over to a new wrapper, as its
/// wrapper does once the agent's processes stopped, and end its execution.
/// Returns the pending restart.
async fn hand_over(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    execution: &crate::daemon::executions::StartedExecution,
) -> tokio::task::JoinHandle<anyhow::Result<Option<Origin>>> {
    let id = &execution.plan.id;
    track(manager, workspace, id, "claude", true).await;
    let restarting = tokio::spawn({
        let (manager, id) = (manager.clone(), id.to_owned());
        async move { manager.agents.restart_agent(&id, false, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.agents.hand_over_restart(id).await.unwrap();
    // A stop sees the restart before the execution's record goes, so no
    // moment shows it neither.
    assert!(
        manager
            .agents
            .renewing_workspaces()
            .await
            .contains(&workspace.id)
    );
    let complete = manager
        .agents
        .finish_execution(&**manager, id.to_owned(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(complete);
    restarting
}

/// How the restart `restarting` ended; the limit is a hang guard only.
async fn answered(
    restarting: tokio::task::JoinHandle<anyhow::Result<Option<Origin>>>,
) -> anyhow::Result<Option<Origin>> {
    tokio::time::timeout(Duration::from_secs(60), restarting)
        .await
        .expect("the restart was never answered")
        .unwrap()
}

/// The restores waiting in the agent queue for `execution`.
async fn restores_of(manager: &Manager, execution: &str) -> usize {
    let entries = manager.agents.queue_entries(manager).await.unwrap();
    entries
        .iter()
        .filter(|entry| entry.execution.as_ref().map(ExecutionId::as_str) == Some(execution))
        .count()
}

/// A stop cancels a restart between its wrappers, whether it comes before
/// the restart records the new wrapper's start or after, and leaves the
/// agent's record for `shoal resume`.
#[tokio::test]
async fn a_stop_cancels_a_restart_between_its_wrappers() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let begin = || {
        manager.agents.begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
    };
    let first = begin().await.unwrap();
    let before = first.plan.id.clone();

    let restarting = hand_over(&manager, &workspace, &first).await;
    // `shoal stop --all` selects the workspace, though no execution or
    // queue entry runs or starts the agent now.
    let targets = manager
        .agents
        .stop_targets(&*manager, crate::protocol::QueuedLaunches::Cancel)
        .await
        .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));
    manager
        .agents
        .cancel_stopped_restores(&*manager, &workspace.id)
        .await
        .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    manager
        .agents
        .restart_in_new_wrapper(&*manager, &Space::from(workspace.clone()), &before, true)
        .await;
    assert_eq!(restores_of(&manager, before.as_str()).await, 0);

    let second = begin().await.unwrap();
    let after = second.plan.id.clone();
    let restarting = hand_over(&manager, &workspace, &second).await;
    manager
        .agents
        .restart_in_new_wrapper(&*manager, &Space::from(workspace.clone()), &after, true)
        .await;
    assert_eq!(restores_of(&manager, after.as_str()).await, 1);
    manager
        .agents
        .cancel_stopped_restores(&*manager, &workspace.id)
        .await
        .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    assert_eq!(restores_of(&manager, after.as_str()).await, 0);

    for id in [&before, &after] {
        assert!(recovery::record_path(&manager.paths, &workspace.id, id).is_file());
    }
}
