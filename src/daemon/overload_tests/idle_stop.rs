//! An idle agent Shoal stops to free memory comes back when an update arrives,
//! even in a done workspace and whatever the overload recovery policy says.
use super::*;
use crate::{daemon::workspace_files::Attacher, state::AgentState};

/// Report the workspace's running agent idle, then hold the workspace and
/// mark it done, and return the agent's execution.
async fn idle_in_held_done_workspace(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> String {
    let id = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(&workspace.name, AgentState::Idle, Some(id.clone()), None)
        .await
        .unwrap();
    manager
        .acquire_hold(
            &workspace.id,
            "app".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, true, false, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    id
}

/// Run the idle stop as the cleanup sweep does, holding an operation.
async fn sweep(manager: &Manager) {
    let _operation = manager.gates.operation().await;
    manager.stop_idle_agents().await.unwrap();
}

#[tokio::test]
async fn an_update_restores_an_idle_agent_stopped_in_a_held_done_workspace() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    idle_in_held_done_workspace(&manager, &workspace).await;
    // Load never recovers here, so only an update can restore the agent.
    set_recovery(&manager, false).await;
    sweep(&manager).await;
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    // The fixture runs no disk monitor; a reading finds the space free.
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    manager
        .send_agent_message(
            &workspace.name,
            "Review requested".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

#[tokio::test]
async fn an_idle_agent_with_a_background_command_keeps_running() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    let id = idle_in_held_done_workspace(&manager, &workspace).await;
    sweep(&manager).await;
    assert!(manager.agent_overload(&id).await.is_none());
    assert!(crate::process::identity::alive(&background).unwrap());
    bounded(manager.stop_workspace(&workspace.name, StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    serving.abort();
}
