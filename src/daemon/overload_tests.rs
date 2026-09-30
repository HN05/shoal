use super::*;
use crate::test_support::{manager, repository};
use std::{future::Future, os::unix::fs::PermissionsExt};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(10), future)
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
                        .any(|record| record.group_id.is_none()))
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
            None
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
