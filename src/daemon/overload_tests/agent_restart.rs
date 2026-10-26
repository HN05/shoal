//! `shoal restart` stops an agent and its wrapper starts the saved session
//! again in the same terminal.
use super::*;

/// The execution of the workspace's only agent.
async fn agent_execution(manager: &Manager, workspace: &crate::model::Workspace) -> String {
    let agents = manager.running_agents().await.unwrap();
    let [agent] = agents.as_slice() else {
        panic!("expected one agent: {agents:?}");
    };
    assert_eq!(agent.workspace_id, workspace.id);
    assert_eq!(agent.agent.as_deref(), Some("fixture"));
    assert!(agent.restorable);
    agent.execution.clone()
}

#[tokio::test]
async fn restart_restores_the_agent_in_its_wrapper_without_waiting_for_healthy_readings() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    bounded(manager.restart_agent(&execution)).await.unwrap();
    // The same wrapper ran the restore command, which exits at once.
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(queue_is_empty(&manager).await);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn a_restart_that_cannot_start_the_agent_leaves_it_for_shoal_resume() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = agent_execution(&manager, &workspace).await;
    fs::remove_file(workspace.path.join("resume.sh")).unwrap();
    let error = bounded(manager.restart_agent(&execution))
        .await
        .unwrap_err();
    assert_eq!(format!("{error}"), "the agent did not start again");
    bounded(launched).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let mut saved = crate::execution::recovery::Saved::load(&manager.paths, &workspace.id).unwrap();
    saved.retain_unattended();
    let [(_, record)] = saved.agents.as_slice() else {
        panic!("expected one stopped agent: {saved:?}");
    };
    let record = crate::execution::recovery::Record::load(record)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("restarted by shoal restart")
    );
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
    let error = bounded(manager.restart_agent(&execution))
        .await
        .unwrap_err();
    assert!(
        format!("{error}").contains("workspace is no longer ready for agent recovery"),
        "{error}"
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}
