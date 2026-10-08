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
    tokio::time::timeout(Duration::from_secs(60), future)
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
    let guard = manager.lock_repository_git(&workspace.repository_id).await;
    let git_gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
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
    assert_eq!(caller.kind(), Some(ExecutionKind::Setup));
    let mut stop = manager.connections.lock().await[caller.execution_id().unwrap()].subscribe();
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
    assert_eq!(Some(started.plan.id.as_str()), caller.execution_id());
    assert_eq!(started.plan.scope_token, token);
    assert!(
        manager
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .await
            .is_ok()
    );
    manager
        .finish_execution_after_scan(
            started.plan.id,
            ExecutionKind::Setup,
            Some(1),
            Some(process::Scan::default()),
        )
        .await
        .unwrap();
    bounded(stopping).await.unwrap().unwrap();
    assert!(manager.caller(&token).await.unwrap().is_none());
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
        // Real process visibility may be incomplete on the host. Failure still
        // closes the connection/scope, retaining any uncertain record as unknown.
        let executions = manager
            .store
            .run(move |db| store::executions(db, &id))
            .await
            .unwrap();
        assert!(
            executions
                .iter()
                .all(|execution| execution.state == ExecutionState::Unknown)
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

fn waiting_process() -> tokio::process::Child {
    tokio::process::Command::new("cat")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("SHOAL_TEST_PROCESS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn unreadable_process_keeps_finished_execution_unknown() {
    for kind in [ExecutionKind::Command, ExecutionKind::Setup] {
        let (_root, manager, workspace) = fixture("exit 0").await;
        let started = manager
            .begin_execution(&workspace.id, None, kind, None)
            .await
            .unwrap();
        fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
        fs::remove_file(workspace.path.join("before.sh")).unwrap();
        let mut child = waiting_process();
        let identity = crate::process::identity::capture(child.id().unwrap())
            .unwrap()
            .unwrap();
        let scan = || crate::process::identity::Scan {
            unreadable: vec![identity.clone()],
            ..Default::default()
        };
        assert!(
            !manager
                .finish_execution_after_scan(started.plan.id.clone(), kind, Some(0), Some(scan()))
                .await
                .unwrap()
        );
        let query_id = started.plan.id.clone();
        let execution = manager
            .store
            .run(move |db| store::find_execution(db, &query_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(execution.state, ExecutionState::Unknown);
        assert!(!manager.execution_connected(&execution.id).await);
        assert!(
            manager
                .caller(&started.plan.scope_token)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        if kind == ExecutionKind::Setup {
            assert_eq!(
                manager.workspace(&workspace.id).await.unwrap().state,
                WorkspaceState::Failed
            );
            assert!(!setup_finished(&manager, &workspace).await);
        }
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(
            manager
                .finish_execution_after_scan(started.plan.id.clone(), kind, Some(0), Some(scan()))
                .await
                .unwrap()
        );
        let query_id = started.plan.id;
        assert!(
            manager
                .store
                .run(move |db| store::find_execution(db, &query_id))
                .await
                .unwrap()
                .is_none()
        );
        if kind == ExecutionKind::Command {
            assert!(
                manager
                    .cleanup_snapshot(&workspace.id)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[tokio::test]
async fn recorded_child_and_unverified_group_block_completion_without_markers() {
    for recorded_child in [true, false] {
        let (_root, manager, workspace) = fixture("exit 0").await;
        let started = manager
            .begin_execution(
                &workspace.id,
                process::capture(std::process::id()).unwrap(),
                ExecutionKind::Setup,
                None,
            )
            .await
            .unwrap();
        let mut child = waiting_process();
        let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
        manager
            .record_execution_child(
                started.plan.id.clone(),
                recorded_child.then_some(identity.clone()),
                identity.pid,
            )
            .await
            .unwrap();
        assert!(
            !manager
                .finish_execution_after_scan(
                    started.plan.id.clone(),
                    ExecutionKind::Setup,
                    Some(0),
                    Some(process::Scan::default())
                )
                .await
                .unwrap()
        );
        assert_eq!(
            manager.workspace(&workspace.id).await.unwrap().state,
            WorkspaceState::Failed
        );
        assert!(!setup_finished(&manager, &workspace).await);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        // The reporting wrapper is still alive but its command/group is gone.
        assert!(
            manager
                .finish_execution_after_scan(
                    started.plan.id,
                    ExecutionKind::Setup,
                    Some(0),
                    Some(process::Scan::default())
                )
                .await
                .unwrap()
        );
    }
}
