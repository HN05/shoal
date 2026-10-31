use super::*;
use crate::test_support::{install_test_script, manager, repository};
use std::{fs, future::Future, sync::Arc};

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
        "hooks.pre_setup = 'before.sh'\n",
    )
    .unwrap();
    let script = workspace.path.join("before.sh");
    install_test_script(&script, &format!("#!/bin/sh\nset -eu\n{hook}\n"));
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
    assert!(!manager.executions.has_connections().await);
    assert!(manager.executions.scopes().await.is_empty());
    assert!(
        manager
            .gates
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .is_ok()
    );
}

#[tokio::test]
async fn registration_publishes_atomically_and_preserves_stop_during_pre_setup() {
    let (_root, manager, workspace) =
        fixture(": > hook-started\nwhile [ ! -f hook-release ]; do sleep 0.01; done").await;
    let guard = manager.gates.lock_git(&workspace.repository_id).await;
    let git_gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    // Pause publication after persistence, with registration still in flight.
    let scopes = manager.executions.scopes().await;
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
    assert!(manager.executions.connections_locked());
    assert!(git_gate.try_lock().is_err());
    assert!(
        manager
            .gates
            .resource_guard(&workspace.id, GuardMode::Shared)
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
            .gates
            .resource_guard(&workspace.id, GuardMode::Shared)
            .is_err()
    );
    let (token, caller) = {
        let scopes = manager.executions.scopes().await;
        let (token, caller) = scopes.iter().next().unwrap();
        (token.clone(), caller.clone())
    };
    assert_eq!(caller.workspace_id, workspace.id);
    assert_eq!(caller.kind(), Some(ExecutionKind::Setup));
    let mut stop = manager
        .executions
        .connections()
        .await
        .stop_channel(caller.execution_id().unwrap())
        .unwrap()
        .subscribe();
    let stopping = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        // Setup's reservation already excludes new executions. Exercise the
        // notification path directly while the wrapper still awaits its plan.
        tokio::spawn(async move {
            manager
                .stop_executions(
                    &id,
                    StopPolicy::RequireCompleteProof,
                    &crate::daemon::workspace::StopRequest::removal(),
                )
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
            .gates
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .is_ok()
    );
    manager
        .finish_execution_after_scan(
            started.plan.id,
            ExecutionKind::Setup,
            Some(1),
            Some(Ok(process::Scan::default())),
        )
        .await
        .unwrap();
    bounded(stopping).await.unwrap().unwrap();
    assert!(manager.caller(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn pre_setup_failure_and_identity_change_close_registration() {
    for (hook, diagnostic) in [
        ("exit 7", "hooks.pre_setup exited with 7"),
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
        assert!(!manager.executions.has_connections().await);
        assert!(manager.executions.scopes().await.is_empty());
        assert!(
            manager
                .gates
                .resource_guard(&workspace.id, GuardMode::Exclusive)
                .is_ok()
        );
    }
}

/// A process waiting in `directory`, carrying the marker of `execution` when
/// one is given.
fn waiting_process(directory: &Path, execution: Option<&str>) -> tokio::process::Child {
    let mut command = tokio::process::Command::new("cat");
    command
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("SHOAL_TEST_PROCESS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(execution) = execution {
        command.env(crate::env::EXECUTION_ID, execution);
    }
    command.spawn().unwrap()
}

/// A process waiting in a group of its own, marked as `execution`'s when
/// given. It runs this test binary, whose environment the scan can read where
/// a system binary's may be hidden.
fn background_process(execution: Option<&str>) -> tokio::process::Child {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1");
    if let Some(execution) = execution {
        command.env(crate::env::EXECUTION_ID, execution);
    }
    command.process_group(0).kill_on_drop(true).spawn().unwrap()
}

#[tokio::test]
async fn unreadable_process_keeps_finished_execution_unknown() {
    for kind in [ExecutionKind::Command, ExecutionKind::Setup] {
        let (_root, manager, workspace) = fixture("exit 0").await;
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        let started = manager
            .begin_execution(&workspace.id, Some(wrapper), kind, None)
            .await
            .unwrap();
        fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
        fs::remove_file(workspace.path.join("before.sh")).unwrap();
        // Reading it again shows the marker where macOS does not hide it.
        let mut child = waiting_process(&workspace.path, Some(&started.plan.id));
        let identity = crate::process::identity::capture(child.id().unwrap())
            .unwrap()
            .unwrap();
        let scan = || crate::process::identity::Scan {
            unreadable: vec![crate::process::identity::Unattributed::alone(
                identity.clone(),
            )],
            ..Default::default()
        };
        assert!(
            !manager
                .finish_execution_after_scan(
                    started.plan.id.clone(),
                    kind,
                    Some(0),
                    Some(Ok(scan()))
                )
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
        assert!(!manager.executions.is_connected(&execution.id).await);
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
                .finish_execution_after_scan(
                    started.plan.id.clone(),
                    kind,
                    Some(0),
                    Some(Ok(scan()))
                )
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
        let mut child = waiting_process(&workspace.path, None);
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
                    Some(Ok(process::Scan::default()))
                )
                .await
                .unwrap()
        );
        assert_eq!(
            manager.workspace(&workspace.id).await.unwrap().state,
            WorkspaceState::Failed
        );
        assert!(!setup_finished(&manager, &workspace).await);
        let error = manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .error
            .unwrap();
        assert!(error.contains("setup command exited successfully (exit 0)"));
        assert!(error.contains(&format!("PID {}", identity.pid)), "{error}");
        assert!(error.contains(if recorded_child {
            "owned processes still running"
        } else {
            "unverified process-group survivors"
        }));
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        // The reporting wrapper is still alive but its command/group is gone.
        assert!(
            manager
                .finish_execution_after_scan(
                    started.plan.id,
                    ExecutionKind::Setup,
                    Some(0),
                    Some(Ok(process::Scan::default()))
                )
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn setup_preserves_inventory_failure_in_its_retained_diagnostic() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let started = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
        .await
        .unwrap();
    assert!(
        !manager
            .finish_execution_after_scan(
                started.plan.id,
                ExecutionKind::Setup,
                Some(0),
                Some(Err(anyhow::anyhow!("inventory fixture refused"))),
            )
            .await
            .unwrap()
    );
    let workspace = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(
        workspace
            .error
            .unwrap()
            .contains("process inventory failed: inventory fixture refused")
    );
}

#[tokio::test]
async fn stopping_allows_the_child_stop_and_completion_budgets() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await?;
    // Pause only after fixture setup, which uses real subprocesses.
    tokio::time::pause();
    let request = StopRequest::manual();
    let stopping =
        manager.stop_executions(&workspace.id, StopPolicy::RequireCompleteProof, &request);
    tokio::pin!(stopping);
    tokio::select! {
        biased;
        result = &mut stopping => bail!("stop ended before notification: {result:?}"),
        changed = running.stop.wait_for(|stop| *stop) => { changed?; }
    }

    // A connected wrapper may spend its grace stopping the child, then the
    // completion budget awaiting the daemon's survivor cleanup and inventories.
    tokio::time::advance(timing::EXECUTION_STOP_GRACE + timing::COMPLETION_ACK_TIMEOUT).await;
    tokio::select! {
        biased;
        result = &mut stopping => bail!("stop ended during valid completion work: {result:?}"),
        changed = running.stop.changed() => { changed?; }
    }
    let id = running.plan.id.clone();
    manager
        .executions
        .set_completing(&id, Completing::Verifying);
    manager
        .finish_execution_after_scan(
            running.plan.id,
            ExecutionKind::Command,
            Some(0),
            Some(Ok(process::Scan::default())),
        )
        .await?;
    stopping.await?;
    // The recorded step goes with the connection.
    assert_eq!(manager.executions.completing(&id), None);
    Ok(())
}

#[tokio::test]
async fn stopping_still_bounds_a_wrapper_that_never_completes() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    // Connected receivers keep the daemon from signalling the recorded child,
    // which is this test process.
    let unstarted = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await?;
    let child = process::capture(std::process::id())?.context("test process identity")?;
    let mut running = Vec::new();
    for _ in 0..2 {
        let started = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await?;
        manager
            .record_execution_child(started.plan.id.clone(), Some(child.clone()), child.pid)
            .await?;
        running.push(started);
    }
    let [still_running, verifying] = running.as_slice() else {
        bail!("expected two running executions");
    };
    manager
        .executions
        .set_completing(&verifying.plan.id, Completing::Verifying);
    tokio::time::pause();
    let started = Instant::now();
    let error = manager
        .stop_executions(
            &workspace.id,
            StopPolicy::RequireCompleteProof,
            &StopRequest::manual(),
        )
        .await
        .expect_err("a wrapper that never completes must time out");
    // Each execution the stop waited on is named with the step it was on.
    let message = error.to_string();
    assert!(
        message.starts_with("timed out waiting for workspace processes to stop: "),
        "{message}"
    );
    for expected in [
        format!("execution {}: command not started", unstarted.plan.id),
        format!("execution {}: command still running", still_running.plan.id),
        format!(
            "execution {}: command exited, verifying that its processes stopped",
            verifying.plan.id
        ),
    ] {
        assert!(message.contains(&expected), "{message}");
    }
    assert!(
        (timing::WORKSPACE_STOP_TIMEOUT
            ..=timing::WORKSPACE_STOP_TIMEOUT + timing::WORKSPACE_STOP_POLL_INTERVAL)
            .contains(&started.elapsed())
    );
    assert_eq!(manager.workspace_executions(&workspace.id).await?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_connected_executions_and_refuses_new_ones() {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = running.plan.id.clone();
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_for_shutdown().await }
    });
    bounded(running.stop.wait_for(|stop| *stop)).await.unwrap();
    assert!(
        manager
            .executions
            .stop_request(&id)
            .await
            .is_some_and(|request| request.save == Save::Everything)
    );
    let error = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("shutting down"), "{error:#}");
    manager
        .finish_execution(id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    bounded(shutdown).await.unwrap();
}

#[tokio::test]
async fn reattachment_requires_the_recorded_identities_and_scope_token() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let token = started.plan.scope_token.clone();
    manager
        .record_execution_child(id.clone(), Some(wrapper.clone()), wrapper.pid)
        .await
        .unwrap();
    drop(started);
    // A restart forgets connections and scopes and marks the record unknown.
    manager.detach_execution(&id).await;
    manager
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();
    assert!(manager.caller(&token).await.unwrap().is_none());
    let request = Reattach {
        execution: id.clone(),
        wrapper: wrapper.clone(),
        child: Some(wrapper.clone()),
        group_id: wrapper.pid,
        scope_token: token.clone(),
        agent: Some("codex".into()),
        recover: true,
        running_ms: 0,
        awaiting_restore: None,
        between_runs: false,
    };
    let stranger = Identity {
        pid: wrapper.pid,
        birth: "another process".into(),
        unique: None,
    };
    for (request, refusal) in [
        (
            Reattach {
                execution: Uuid::new_v4().to_string(),
                ..request.clone()
            },
            "execution record is missing",
        ),
        (
            Reattach {
                scope_token: Uuid::new_v4().to_string(),
                ..request.clone()
            },
            "scope token does not match",
        ),
        (
            Reattach {
                child: None,
                ..request.clone()
            },
            "process identity does not match",
        ),
        (
            Reattach {
                group_id: wrapper.pid + 1,
                ..request.clone()
            },
            "process identity does not match",
        ),
        (
            Reattach {
                wrapper: stranger,
                ..request.clone()
            },
            "wrapper is no longer alive",
        ),
    ] {
        let error = manager.reattach_execution(&request).await.unwrap_err();
        assert!(format!("{error:#}").contains(refusal), "{error:#}");
    }
    assert!(!manager.executions.is_connected(&id).await);
    let reattached = manager.reattach_execution(&request).await.unwrap();
    assert_eq!(reattached.workspace.id, workspace.id);
    assert!(manager.executions.is_connected(&id).await);
    let caller = manager.caller(&token).await.unwrap().unwrap();
    assert_eq!(caller.execution_id(), Some(id.as_str()));
    assert_eq!(caller.kind(), Some(ExecutionKind::Command));
    assert!(manager.executions.with_agent(&id, |_| ()).await.is_some());
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(inspection.executions[0].state, ExecutionState::Running);
    let error = manager.reattach_execution(&request).await.unwrap_err();
    assert!(error.to_string().contains("already connected"), "{error:#}");
}

/// A wrapper waiting to restore its stopped agent reattaches after the daemon
/// forgot the stopped child, and the agent stays stopped for the restore.
#[tokio::test]
async fn a_wrapper_awaiting_a_restore_reattaches_after_its_child_was_forgotten() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let request = Reattach {
        execution: id.clone(),
        wrapper: wrapper.clone(),
        child: Some(wrapper.clone()),
        group_id: wrapper.pid,
        scope_token: started.plan.scope_token.clone(),
        agent: Some("codex".into()),
        recover: true,
        running_ms: 0,
        awaiting_restore: None,
        between_runs: false,
    };
    drop(started);
    // The record names no child, as after a proven pause, and a restart
    // marks it unknown.
    manager.detach_execution(&id).await;
    manager
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();
    let error = manager.reattach_execution(&request).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("process identity does not match"),
        "{error:#}"
    );
    let protection = crate::protocol::Protection::load("test memory pressure");
    let reattached = manager
        .reattach_execution(&Reattach {
            awaiting_restore: Some(protection.clone()),
            ..request
        })
        .await
        .unwrap();
    assert!(*reattached.stop.borrow());
    assert_eq!(manager.agent_overload(&id).await, Some(protection));
}

/// A demo's wrapper between runs names its exited child, which the daemon
/// may already have forgotten when it went away before answering.
#[tokio::test]
async fn a_wrapper_between_runs_reattaches_after_its_child_was_forgotten() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let request = Reattach {
        execution: id.clone(),
        wrapper: wrapper.clone(),
        child: Some(wrapper.clone()),
        group_id: wrapper.pid,
        scope_token: started.plan.scope_token.clone(),
        agent: None,
        recover: false,
        running_ms: 0,
        awaiting_restore: None,
        between_runs: false,
    };
    drop(started);
    manager.detach_execution(&id).await;
    // The daemon released the exited child, then went away before answering.
    let forget = id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
                [forget],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = manager.reattach_execution(&request).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("process identity does not match"),
        "{error:#}"
    );
    let reattached = manager
        .reattach_execution(&Reattach {
            between_runs: true,
            ..request
        })
        .await
        .unwrap();
    assert_eq!(reattached.workspace.id, workspace.id);
}

#[tokio::test]
async fn setup_executions_cannot_reattach() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            Some(wrapper.clone()),
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap();
    manager.detach_execution(&started.plan.id).await;
    let error = manager
        .reattach_execution(&Reattach {
            execution: started.plan.id.clone(),
            wrapper: wrapper.clone(),
            child: None,
            group_id: wrapper.pid,
            scope_token: started.plan.scope_token.clone(),
            agent: None,
            recover: false,
            running_ms: 0,
            awaiting_restore: None,
            between_runs: false,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("scope token"), "{error:#}");
}

#[tokio::test]
async fn an_idle_agent_with_work_outside_its_group_keeps_a_working_turn() {
    use crate::{
        execution::recovery::{Record, record_path, save_record},
        state::AgentState,
    };
    for background in [false, true] {
        let (_root, manager, workspace) = fixture("exit 0").await;
        fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
        let started = manager
            .begin_execution(
                &workspace.id,
                process::capture(std::process::id()).unwrap(),
                ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        let id = started.plan.id;
        manager
            .set_agent_state(&workspace.name, AgentState::Idle, Some(id.clone()), None)
            .await
            .unwrap();
        // A signal stop saved the agent, which leaves a background command
        // it started running in a process group of its own.
        save_record(&manager.paths, &workspace.id, &id, "claude", None, None).unwrap();
        let mut command = background_process(background.then_some(id.as_str()));

        // The agent's exit stops that command before the execution ends.
        manager.stop_exited_processes(&id).await;
        command.kill().await.unwrap();
        command.wait().await.unwrap();
        manager
            .finish_execution(id.clone(), ExecutionKind::Command, Some(0))
            .await
            .unwrap();

        let record = Record::load(&record_path(&manager.paths, &workspace.id, &id))
            .unwrap()
            .unwrap();
        let expected = if background {
            AgentState::Working
        } else {
            AgentState::Idle
        };
        assert_eq!(record.turn, Some(expected), "background work: {background}");
    }
}
