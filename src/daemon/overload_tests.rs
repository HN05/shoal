use super::*;
use crate::test_support::{manager, repository};
use std::{future::Future, os::unix::fs::PermissionsExt};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    // A hang guard, not a deadline for process scans on a shared CI host.
    timeout(Duration::from_secs(60), future)
        .await
        .expect("overload exchange timed out")
}

async fn fixture() -> (
    tempfile::TempDir,
    Arc<Manager>,
    crate::model::Workspace,
    tokio::task::JoinHandle<()>,
) {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "load".into(), None, None, None)
        .await
        .unwrap();
    let script = workspace.path.join("resume.sh");
    fs::write(&script, "#!/bin/sh\nprintf restored > restored\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\nfixture = ['./resume.sh']\n",
    )
    .unwrap();
    let listener = UnixListener::bind(&manager.paths.socket).unwrap();
    let server = Server {
        manager: manager.clone(),
        started: Instant::now(),
        managed: false,
        shutdown: watch::channel(false).0,
    };
    let serving = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let server = server.clone();
                    clients.spawn(async move { serve(stream, server).await });
                }
                Some(result) = clients.join_next(), if !clients.is_empty() => { let _ = result.unwrap(); }
            }
        }
    });
    (root, manager, workspace, serving)
}

async fn wait_started(workspace: &crate::model::Workspace) {
    bounded(async {
        while !workspace.path.join("started").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

async fn wait_paused(manager: &Manager, workspace: &crate::model::Workspace) {
    bounded(async {
        loop {
            let id = workspace.id.clone();
            let paused = manager
                .store
                .run(move |db| {
                    Ok(store::executions(db, &id)?
                        .iter()
                        .any(|record| record.wrapper.is_some() && record.group_id.is_none()))
                })
                .await
                .unwrap();
            if paused {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

fn launch(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> tokio::task::JoinHandle<Result<i32>> {
    let paths = manager.paths.clone();
    let id = workspace.id.clone();
    tokio::spawn(async move {
        crate::execution::run(
            &paths,
            id,
            vec![
                "sh".into(),
                "-c".into(),
                "touch started; trap 'exit 0' TERM; while :; do sleep 1; done".into(),
            ],
            Some("fixture".into()),
        )
        .await
    })
}

#[tokio::test]
async fn manual_pause_preserves_work_and_leases_and_requires_manual_resume() {
    let (_root, manager, workspace, serving) = fixture().await;
    manager
        .acquire_port(
            &workspace.id,
            "web".into(),
            crate::daemon::ports::PortRequest::default(),
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
    let unrelated = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    let ctx = crate::cli::context::Context::new(manager.paths.clone(), true);
    crate::cli::client::request::<()>(
        &manager.paths,
        Method::WorkspacePause {
            workspace: workspace.id.clone(),
            execution: None,
        },
    )
    .await
    .unwrap();
    bounded(launched).await.unwrap().unwrap();
    let retained = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(retained.workspace.state, workspace.state);
    assert_eq!(retained.ports.len(), 1);
    assert_eq!(retained.executions.len(), 1);
    assert!(!*unrelated.stop.borrow());
    assert_eq!(
        fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    manager.recovery_ready.send_replace(Some(0));
    assert!(!workspace.path.join("restored").exists());
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\nfixture = ['./resume.sh']\n",
    )
    .unwrap();
    assert_eq!(
        bounded(crate::cli::commands::resume::run(
            &ctx,
            Some(workspace.id.clone()),
            None,
            false
        ))
        .await
        .unwrap(),
        0
    );
    assert!(workspace.path.join("restored").exists());
    manager
        .finish_execution(unrelated.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    serving.abort();
}

#[tokio::test]
async fn manual_pause_selects_only_the_requested_connected_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    let id = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    let unrelated = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(&unrelated.plan.id, "other", &workspace.name)
        .await;
    assert!(
        manager
            .pause_workspace_agents(&workspace.id, Some("missing"))
            .await
            .is_err()
    );
    assert!(!*unrelated.stop.borrow());
    let token = unrelated.plan.scope_token.clone();
    let mut method = Method::WorkspacePause {
        workspace: workspace.id.clone(),
        execution: Some(id.clone()),
    };
    assert!(
        scope::authorize(&manager, Some(&token), &mut method)
            .await
            .is_err()
    );
    bounded(manager.pause_workspace_agents(&workspace.id, Some(&id)))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(!workspace.path.join("restored").exists());
    assert!(!*unrelated.stop.borrow());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .iter()
            .any(|execution| execution.id == unrelated.plan.id)
    );
    manager
        .finish_execution(unrelated.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    serving.abort();
}

#[tokio::test]
async fn manual_pause_cancels_automatic_restore_of_an_overloaded_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(manager.stop_agent_for_overload("test pressure").await);
    wait_paused(&manager, &workspace).await;
    bounded(manager.pause_workspace_agents(&workspace.id, None))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(!workspace.path.join("restored").exists());
    serving.abort();
}

#[tokio::test]
async fn overload_restores_the_configured_session_and_keeps_waiting_execution_owned() {
    let (_root, manager, workspace, serving) = fixture().await;
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf restored > restored\nexit 7\n",
    )
    .unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    let epoch = manager
        .recovery_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    // Stale healthy evidence from before the overload must not release it.
    manager
        .recovery_ready
        .send_replace(Some(epoch.wrapping_sub(1)));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!workspace.path.join("restored").exists());
    manager.recovery_ready.send_replace(Some(epoch));
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 7);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::AgentResumed)
    );
    serving.abort();
}

#[tokio::test]
async fn manual_stop_cancels_waiting_recovery_and_leaves_a_restore_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&workspace.id))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    let records = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let text = fs::read_to_string(&records[0]).unwrap();
    let record: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(record, serde_json::json!({"agent": "fixture"}));
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    serving.abort();
}

#[tokio::test]
async fn failed_restore_expansion_does_not_block_launch_or_manual_recovery_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET base_ref='refs/heads/missing' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\nfixture = ['./resume.sh', '{diff_base}']\n",
    )
    .unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    serving.abort();
}

#[tokio::test]
async fn discard_clears_pending_recovery_without_launching_an_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert!(
        crate::cli::commands::resume::run(
            &crate::cli::context::Context::new(manager.paths.clone(), true),
            Some(workspace.id.clone()),
            None,
            true,
        )
        .await
        .is_err()
    );
    bounded(manager.stop_workspace(&workspace.id))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    // Discard does not depend on valid current restore configuration.
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\nfixture = ['./missing', '{diff_base}']\n",
    )
    .unwrap();
    assert_eq!(
        crate::cli::commands::resume::run(
            &crate::cli::context::Context::new(manager.paths.clone(), true),
            Some(workspace.id.clone()),
            None,
            true,
        )
        .await
        .unwrap(),
        0
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(!workspace.path.join("restored").exists());
    assert!(workspace.path.is_dir());
    serving.abort();
}

#[tokio::test]
async fn refused_recovery_finishes_the_stopped_execution_and_retains_manual_recovery() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    manager
        .set_state(&workspace.id, crate::state::WorkspaceState::Failed, None)
        .await
        .unwrap();
    let epoch = manager
        .recovery_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    manager.recovery_ready.send_replace(Some(epoch));
    bounded(launched).await.unwrap().unwrap();
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .all(|event| !event.message.contains("run shoal doctor"))
    );
    serving.abort();
}

#[tokio::test]
async fn recovery_disconnect_closes_the_connection_without_forgetting_ownership() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    launched.abort();
    bounded(async {
        loop {
            let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
            if inspection
                .executions
                .iter()
                .any(|execution| execution.state == crate::state::ExecutionState::Unknown)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(!workspace.path.join("restored").exists());
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        !reopened
            .stop_agent_for_overload("test overload after restart")
            .await
    );
    assert!(
        !reopened
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    reopened.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&workspace.id))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    let directory = manager.paths.workspace_state(&workspace.id);
    let record = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .unwrap();
    assert_eq!(
        fs::metadata(&record).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf changed > restored\nexit 7\n",
    )
    .unwrap();
    let unrelated = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let ctx = crate::cli::context::Context::new(manager.paths.clone(), true);
    assert_eq!(
        bounded(crate::cli::commands::resume::run(
            &ctx,
            Some(workspace.id.clone()),
            None,
            false
        ))
        .await
        .unwrap(),
        7
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "changed"
    );
    assert!(!record.exists());
    manager
        .finish_execution(unrelated.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    serving.abort();
}
