//! A restore waiting for an update to an agent stopped between turns keeps
//! its workspace from no cleanup; removal cancels it and keeps the agent for
//! `shoal work restore`.
use super::*;
use crate::{model::CleanupReason, state::AgentState};

#[tokio::test]
async fn cleanup_removes_a_workspace_whose_idle_agent_waits_for_an_update() {
    let (_root, manager, workspace, serving) = fixture().await;
    ignore_fixture_files(&workspace);
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = stop_agent_in(&manager, &workspace, AgentState::Idle).await;
    wait_restore_waits(&manager, &id).await;
    let snapshot = manager
        .cleanup_candidacy(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        manager
            .merged_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    bounded(manager.remove_on_request(&workspace.id, snapshot))
        .await
        .unwrap();
    // Removal cancelled the restore, so the waiting wrapper exits.
    bounded(launched).await.unwrap().unwrap();
    assert!(queue_is_empty(&manager).await);
    let removed = manager.removed_workspaces().await.unwrap();
    let agents = &removed[0].agents;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].execution, id);
    assert_eq!(agents[0].record.turn, Some(AgentState::Idle));
    let restored = manager.restore_workspace(&workspace.name).await.unwrap();
    let stopped =
        crate::execution::recovery::stopped_agents(&manager.paths, &restored.opened.workspace.id)
            .unwrap();
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].execution, id);
    serving.abort();
}

#[tokio::test]
async fn a_workspace_being_removed_refuses_messages_for_its_stopped_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    ignore_fixture_files(&workspace);
    let home = &manager.paths.home;
    let (started, release) = (home.join("hook-started"), home.join("release"));
    let hook = home.join("wait-for-release");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\n",
            started.display(),
            release.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some(format!("pre_remove_cmd = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = stop_agent_in(&manager, &workspace, AgentState::Idle).await;
    wait_restore_waits(&manager, &id).await;
    let snapshot = manager
        .cleanup_candidacy(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    let removal = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move { manager.remove_on_request(&id, snapshot).await }
    });
    // The hook runs once removal has reserved the workspace and cancelled
    // the restore, before its final check.
    bounded(async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let refused = manager
        .send_agent_message(&workspace.name, "Review requested".into())
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("being removed"),
        "{refused:#}"
    );
    fs::write(&release, "").unwrap();
    bounded(removal).await.unwrap().unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.exists());
    serving.abort();
}

#[tokio::test]
async fn a_restore_of_an_agent_stopped_mid_turn_keeps_its_workspace() {
    let (_root, manager, workspace, serving) = fixture().await;
    ignore_fixture_files(&workspace);
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = stop_agent_in(&manager, &workspace, AgentState::Working).await;
    wait_restore_waits(&manager, &id).await;
    assert_eq!(
        manager.cleanup_candidacy(&workspace.id).await.unwrap(),
        Err(CleanupReason::Recovering)
    );
    assert!(
        manager
            .merged_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    bounded(manager.stop_workspace(&workspace.name, StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    serving.abort();
}

/// Keep the fixture's configuration and the agent's marker out of Git status,
/// so only the agent decides whether cleanup may remove the workspace.
fn ignore_fixture_files(workspace: &crate::model::Workspace) {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(&workspace.path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let common = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    fs::create_dir_all(common.join("info")).unwrap();
    fs::write(
        common.join("info/exclude"),
        ".shoal.toml\nresume.sh\nstarted\n",
    )
    .unwrap();
}

/// Report `turn` for the workspace's running agent, stop it as overload
/// protection does and return its execution once its restore waits in the
/// queue.
async fn stop_agent_in(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    turn: AgentState,
) -> String {
    let id = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(&workspace.name, turn, Some(id.clone()), None)
        .await
        .unwrap();
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(manager, workspace).await;
    wait_restore_queued(manager).await;
    id
}

/// Waits until the stopped agent's wrapper waits for the queue to admit its
/// restore, which it does once every process of the agent is proven stopped.
#[track_caller]
fn wait_restore_waits<'a>(
    manager: &'a Manager,
    execution: &'a str,
) -> impl Future<Output = ()> + 'a {
    bounded(async move {
        while !manager.agent_queue.restore_waiters().contains(execution) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}
