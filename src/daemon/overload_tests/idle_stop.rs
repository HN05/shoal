//! An idle agent Shoal stops to free memory comes back when an update arrives,
//! even in a done workspace and whatever the overload recovery policy says.
use super::*;
use crate::state::AgentState;

#[tokio::test]
async fn an_update_restores_an_idle_agent_stopped_in_a_held_done_workspace() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(&workspace.name, AgentState::Idle, Some(id), None)
        .await
        .unwrap();
    manager
        .acquire_hold(&workspace.id, "app".into(), None)
        .await
        .unwrap();
    manager.mark_done(&workspace.id, true, false).await.unwrap();
    // Load never recovers here, so only an update can restore the agent.
    set_recovery(&manager, false).await;
    let sweep = manager.gates.operation().await;
    manager.stop_idle_agents().await.unwrap();
    drop(sweep);
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    // The fixture runs no disk monitor; a reading finds the space free.
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    manager
        .send_agent_message(&workspace.name, "Review requested".into(), Vec::new())
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}
