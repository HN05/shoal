use super::*;
use crate::test_support::{manager, repository};
use std::{fs, future::Future, os::unix::fs::PermissionsExt, sync::Arc};

async fn fixture(hook: &str) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "execution".into(), None, None, None)
        .await
        .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "pre_setup_cmd = 'before.sh'\n",
    )
    .unwrap();
    let script = workspace.path.join("before.sh");
    fs::write(&script, format!("#!/bin/sh\nset -eu\n{hook}\n")).unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    (root, manager, workspace)
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("execution operation timed out")
}

async fn setup_finished(manager: &Manager, workspace: &Workspace) -> bool {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| store::setup_finished(db, &id))
        .await
        .unwrap()
}

#[tokio::test]
async fn registration_rolls_back_setup_reservation_when_insertion_fails() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let was_finished = setup_finished(&manager, &workspace).await;
    manager
        .store
        .run(|db| {
            db.execute_batch(
                "CREATE TRIGGER reject_execution BEFORE INSERT ON executions
                 BEGIN SELECT RAISE(ABORT, 'injected registration failure'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("injected registration failure"));
    let after = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(after.state, workspace.state);
    assert_eq!(setup_finished(&manager, &workspace).await, was_finished);
    assert!(manager.connections.lock().await.is_empty());
    assert!(manager.scopes.lock().await.is_empty());
    assert!(
        manager
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn registration_publishes_atomically_and_preserves_stop_during_pre_setup() {
    let (_root, manager, workspace) =
        fixture(": > hook-started\nwhile [ ! -f hook-release ]; do sleep 0.01; done").await;
    // Pause publication after persistence, with registration still in flight.
    let scopes = manager.scopes.lock().await;
    let starting = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move {
            manager
                .begin_execution(&id, None, ExecutionKind::Setup, None)
                .await
        })
    };
    bounded(async {
        loop {
            let id = workspace.id.clone();
            let executions = manager
                .store
                .run(move |db| store::executions(db, &id))
                .await
                .unwrap();
            if !executions.is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(manager.connections.try_lock().is_err());
    let git_gate = manager.git_gate(&workspace.repository_id).await;
    assert!(git_gate.try_lock().is_err());
    assert!(
        manager
            .resource_guard(&workspace.id, GuardMode::Shared)
            .await
            .is_err()
    );
    drop(scopes);
    bounded(async {
        while !workspace.path.join("hook-started").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(git_gate.try_lock().is_ok());
    assert!(
        manager
            .resource_guard(&workspace.id, GuardMode::Shared)
            .await
            .is_err()
    );
    let (token, caller) = {
        let scopes = manager.scopes.lock().await;
        let (token, caller) = scopes.iter().next().unwrap();
        (token.clone(), caller.clone())
    };
    assert_eq!(caller.workspace_id, workspace.id);
    assert_eq!(caller.kind, ExecutionKind::Setup);
    let mut stop = manager.connections.lock().await[&caller.execution_id].subscribe();
    let stopping = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        // Setup's reservation already excludes new executions. Exercise the
        // notification path directly while the wrapper still awaits its plan.
        tokio::spawn(async move {
            manager
                .stop_executions(&id, StopPolicy::RequireCompleteProof)
                .await
        })
    };
    bounded(stop.changed()).await.unwrap();
    fs::write(workspace.path.join("hook-release"), "").unwrap();
    let started = bounded(starting).await.unwrap().unwrap();
    assert!(*started.stop.borrow());
    assert_eq!(started.plan.id, caller.execution_id);
    assert_eq!(started.plan.scope_token, token);
    assert!(
        manager
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .await
            .is_ok()
    );
    manager
        .finish_execution(started.plan.id, ExecutionKind::Setup, Some(1))
        .await
        .unwrap();
    bounded(stopping).await.unwrap().unwrap();
    assert!(manager.caller(&token).await.is_none());
}

#[tokio::test]
async fn pre_setup_failure_and_identity_change_close_registration() {
    for (hook, diagnostic) in [
        ("exit 7", "pre_setup_cmd exited with 7"),
        ("mv .git .git.saved", "not a git repository"),
    ] {
        let (_root, manager, workspace) = fixture(hook).await;
        let error = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(diagnostic), "{error:#}");
        let after = manager.workspace(&workspace.id).await.unwrap();
        assert_eq!(after.state, WorkspaceState::Failed);
        assert!(!setup_finished(&manager, &workspace).await);
        assert_eq!(after.error.as_deref(), Some(format!("{error:#}").as_str()));
        let id = workspace.id.clone();
        assert!(
            manager
                .store
                .run(move |db| store::executions(db, &id))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(manager.connections.lock().await.is_empty());
        assert!(manager.scopes.lock().await.is_empty());
        assert!(
            manager
                .resource_guard(&workspace.id, GuardMode::Exclusive)
                .await
                .is_ok()
        );
    }
}
