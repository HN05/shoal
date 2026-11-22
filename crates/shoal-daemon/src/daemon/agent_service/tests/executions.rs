use crate::daemon::agents::ProtectionExt;
use crate::daemon::executions::stopping::StopPolicy;
use crate::daemon::executions::*;
use crate::daemon::test_support::{install_test_script, manager, repository};
use crate::daemon::{gates::GuardMode, manager::Manager};
use crate::execution::recovery::RecordExt;
use crate::{
    daemon::store,
    model::Workspace,
    process::identity::{self as process, Identity},
    protocol::Reattach,
    state::{ExecutionState, WorkspaceState},
};
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::ExecutionKind;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use shoal_store::lifecycle::Reserved;
use std::{fs, future::Future, sync::Arc};
use std::{path::Path, time::Duration};
use tokio::time::sleep;

pub(super) async fn fixture(hook: &str) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    install_test_script(&script, format!("#!/bin/sh\nset -eu\n{hook}\n"));
    (root, manager, workspace)
}

pub(super) async fn bounded<T>(future: impl Future<Output = T>) -> T {
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
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("injected registration failure"));
    let after = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(after.state, workspace.state);
    assert_eq!(setup_finished(&manager, &workspace).await, was_finished);
    assert!(!manager.agents.executions().has_connections().await);
    assert!(manager.agents.executions().scopes().await.is_empty());
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
    let guard = manager.gates.lock_git(workspace.repository()).await;
    let git_gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    drop(guard);
    // Pause publication after persistence, with registration still in flight.
    let scopes = manager.agents.executions().scopes().await;
    let starting = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move {
            manager
                .agents
                .begin_execution(&*manager, id.as_str(), None, ExecutionKind::Setup, None)
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
    assert!(manager.agents.executions().connections_locked());
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
        let scopes = manager.agents.executions().scopes().await;
        let (token, caller) = scopes.iter().next().unwrap();
        (token.clone(), caller.clone())
    };
    assert_eq!(caller.space_id, workspace.id);
    assert_eq!(caller.kind(), Some(ExecutionKind::Setup));
    let mut stop = manager
        .agents
        .executions()
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
                .agents
                .stop_executions(
                    &*manager,
                    &id,
                    StopPolicy::RequireCompleteProof,
                    &crate::daemon::executions::StopRequest::removal(),
                )
                .await
        })
    };
    bounded(stop.changed()).await.unwrap();
    fs::write(workspace.path.join("hook-release"), "").unwrap();
    let started = bounded(starting).await.unwrap().unwrap();
    assert!(*started.stop.borrow());
    assert_eq!(Some(&started.plan.id), caller.execution_id());
    assert_eq!(started.plan.scope_token, token);
    assert!(
        manager
            .gates
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .is_ok()
    );
    manager
        .agents
        .finish_execution_after_scan(
            &*manager,
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
    for hook in ["exit 7", "mv .git .git.saved"] {
        let (_root, manager, workspace) = fixture(hook).await;
        let error = manager
            .agents
            .begin_execution(
                &*manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Setup,
                None,
            )
            .await
            .unwrap_err();
        let after = manager.workspace_by_id(&workspace.id).await.unwrap();
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
        assert!(!manager.agents.executions().has_connections().await);
        assert!(manager.agents.executions().scopes().await.is_empty());
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
    let mut command = crate::daemon::test_support::marked_child();
    command.env_clear().env("SHOAL_TEST_PROCESS", "1");
    if let Some(execution) = execution {
        command.env(crate::env::EXECUTION_ID, execution);
    }
    crate::daemon::test_support::spawn_marked_child(command.process_group(0))
}

#[tokio::test]
async fn unreadable_process_keeps_finished_execution_unknown() {
    for kind in [ExecutionKind::Command, ExecutionKind::Setup] {
        let (_root, manager, workspace) = fixture("exit 0").await;
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        let started = manager
            .agents
            .begin_execution(&*manager, workspace.id.as_str(), Some(wrapper), kind, None)
            .await
            .unwrap();
        fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
        fs::remove_file(workspace.path.join("before.sh")).unwrap();
        // Reading it again shows the marker where macOS does not hide it.
        let mut child = waiting_process(&workspace.path, Some(started.plan.id.as_str()));
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
                .agents
                .finish_execution_after_scan(
                    &*manager,
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
        assert!(
            !manager
                .agents
                .executions()
                .is_connected(&execution.id)
                .await
        );
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
                manager.workspace_by_id(&workspace.id).await.unwrap().state,
                WorkspaceState::Failed
            );
            assert!(!setup_finished(&manager, &workspace).await);
        }
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(
            manager
                .agents
                .finish_execution_after_scan(
                    &*manager,
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
            .agents
            .begin_execution(
                &*manager,
                workspace.id.as_str(),
                process::capture(std::process::id()).unwrap(),
                ExecutionKind::Setup,
                None,
            )
            .await
            .unwrap();
        let mut child = waiting_process(&workspace.path, None);
        let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
        manager
            .agents
            .record_execution_child(
                started.plan.id.clone(),
                recorded_child.then_some(identity.clone()),
                identity.pid,
            )
            .await
            .unwrap();
        assert!(
            !manager
                .agents
                .finish_execution_after_scan(
                    &*manager,
                    started.plan.id.clone(),
                    ExecutionKind::Setup,
                    Some(0),
                    Some(Ok(process::Scan::default()))
                )
                .await
                .unwrap()
        );
        assert_eq!(
            manager.workspace_by_id(&workspace.id).await.unwrap().state,
            WorkspaceState::Failed
        );
        assert!(!setup_finished(&manager, &workspace).await);
        let error = manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .part
            .error
            .unwrap();
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
                .agents
                .finish_execution_after_scan(
                    &*manager,
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
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap();
    assert!(
        !manager
            .agents
            .finish_execution_after_scan(
                &*manager,
                started.plan.id,
                ExecutionKind::Setup,
                Some(0),
                Some(Err(anyhow::anyhow!("inventory fixture refused"))),
            )
            .await
            .unwrap()
    );
    let workspace = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(
        workspace
            .part
            .error
            .unwrap()
            .contains("inventory fixture refused")
    );
}

#[tokio::test]
async fn reattachment_requires_the_recorded_identities_and_scope_token() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let token = started.plan.scope_token.clone();
    manager
        .agents
        .record_execution_child(id.clone(), Some(wrapper.clone()), wrapper.pid)
        .await
        .unwrap();
    drop(started);
    // A restart forgets connections and scopes and marks the record unknown.
    manager.agents.detach_execution(&id).await;
    crate::daemon::quarantine::interrupted_operations(&manager.store)
        .await
        .unwrap();
    assert!(manager.caller(&token).await.unwrap().is_none());
    let request = Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
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
        reports_status: false,
    };
    let stranger = Identity {
        pid: wrapper.pid,
        birth: "another process".into(),
        unique: None,
    };
    for request in [
        Reattach {
            execution: ExecutionId::generate(),
            ..request.clone()
        },
        Reattach {
            scope_token: ScopeToken::generate(),
            ..request.clone()
        },
        Reattach {
            child: None,
            ..request.clone()
        },
        Reattach {
            group_id: wrapper.pid + 1,
            ..request.clone()
        },
        Reattach {
            wrapper: stranger,
            ..request.clone()
        },
    ] {
        manager
            .agents
            .reattach_execution(&*manager, &request)
            .await
            .unwrap_err();
    }
    assert!(!manager.agents.executions().is_connected(&id).await);
    let reattached = manager
        .agents
        .reattach_execution(&*manager, &request)
        .await
        .unwrap();
    assert_eq!(&reattached.space.id, &workspace.id);
    assert!(manager.agents.executions().is_connected(&id).await);
    let caller = manager.caller(&token).await.unwrap().unwrap();
    assert_eq!(caller.execution_id(), Some(&id));
    assert_eq!(caller.kind(), Some(ExecutionKind::Command));
    assert!(
        manager
            .agents
            .executions()
            .with_agent(&id, |_| ())
            .await
            .is_some()
    );
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.executions[0].state, ExecutionState::Running);
    manager
        .agents
        .reattach_execution(&*manager, &request)
        .await
        .unwrap_err();
}

/// A wrapper waiting to restore its stopped agent reattaches after the daemon
/// forgot the stopped child, and the agent stays stopped for the restore.
#[tokio::test]
async fn a_wrapper_awaiting_a_restore_reattaches_after_its_child_was_forgotten() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let request = Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
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
        reports_status: false,
    };
    drop(started);
    // The record names no child, as after a proven pause, and a restart
    // marks it unknown.
    manager.agents.detach_execution(&id).await;
    crate::daemon::quarantine::interrupted_operations(&manager.store)
        .await
        .unwrap();
    manager
        .agents
        .reattach_execution(&*manager, &request)
        .await
        .unwrap_err();
    let protection = crate::model::Protection::load("test memory pressure");
    let reattached = manager
        .agents
        .reattach_execution(
            &*manager,
            &Reattach {
                awaiting_restore: Some(protection.clone()),
                ..request
            },
        )
        .await
        .unwrap();
    assert!(*reattached.stop.borrow());
    assert_eq!(manager.agents.agent_overload(&id).await, Some(protection));
}

/// A demo's wrapper between runs names its exited child, which the daemon
/// may already have forgotten when it went away before answering.
#[tokio::test]
async fn a_wrapper_between_runs_reattaches_after_its_child_was_forgotten() {
    let (_root, manager, workspace) = fixture("true").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let request = Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
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
        reports_status: false,
    };
    drop(started);
    manager.agents.detach_execution(&id).await;
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
    manager
        .agents
        .reattach_execution(&*manager, &request)
        .await
        .unwrap_err();
    let reattached = manager
        .agents
        .reattach_execution(
            &*manager,
            &Reattach {
                between_runs: true,
                ..request
            },
        )
        .await
        .unwrap();
    assert_eq!(&reattached.space.id, &workspace.id);
}

#[tokio::test]
async fn setup_executions_cannot_reattach() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap();
    manager.agents.detach_execution(&started.plan.id).await;
    manager
        .agents
        .reattach_execution(
            &*manager,
            &Reattach {
                narrowing: Default::default(),
                mode: None,
                model: Default::default(),
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
                reports_status: false,
            },
        )
        .await
        .unwrap_err();
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
            .agents
            .begin_execution(
                &*manager,
                workspace.id.as_str(),
                process::capture(std::process::id()).unwrap(),
                ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        let id = started.plan.id;
        manager
            .agents
            .set_agent_state(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                AgentState::Idle,
                Some(id.clone()),
                None,
            )
            .await
            .unwrap();
        // A signal stop saved the agent, which leaves a background command
        // it started running in a process group of its own.
        save_record(
            &manager.paths,
            &workspace.id,
            &id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            None,
            None,
        )
        .unwrap();
        let mut command = background_process(background.then_some(id.as_str()));

        // The agent's exit stops that command before the execution ends.
        manager.agents.stop_exited_processes(&*manager, &id).await;
        command.kill().await.unwrap();
        command.wait().await.unwrap();
        manager
            .agents
            .finish_execution(&*manager, id.clone(), ExecutionKind::Command, Some(0))
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

/// What a command's registration decides for its workspace in `state`.
async fn command_reservation(
    manager: &Manager,
    workspace: &Workspace,
    state: WorkspaceState,
) -> Result<()> {
    manager.set_state(&workspace.id, state, None).await?;
    let id = workspace.id.clone();
    manager
        .store
        .write(move |tx| ExecutionKind::Command.reserve(tx, &id, None))
        .await
}

#[tokio::test]
async fn a_command_waits_only_for_a_state_that_passes() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let updating = command_reservation(&manager, &workspace, WorkspaceState::Updating).await;
    assert!(updating.unwrap_err().is::<Reserved>());
    for lasting in [WorkspaceState::Failed, WorkspaceState::Removing] {
        let refused = command_reservation(&manager, &workspace, lasting)
            .await
            .unwrap_err();
        assert!(!refused.is::<Reserved>(), "{lasting}: {refused:#}");
        let reason = crate::refusal::Reason::of(&refused);
        assert_eq!(reason, Some(crate::refusal::Reason::NotReady), "{lasting}");
    }
}

/// A command's start in `workspace`, begun while `held` holds it, once the
/// start waits for that to end or has given up.
async fn command_started_during(
    manager: &Arc<Manager>,
    workspace: &Workspace,
    held: WorkspaceState,
) -> tokio::task::JoinHandle<Result<StartedExecution>> {
    manager
        .reserve_lifecycle(&workspace.id, held)
        .await
        .unwrap();
    let watchers = manager.store.change_watchers();
    let start = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move {
            (manager.agents.begin_execution(
                &*manager,
                id.as_str(),
                None,
                ExecutionKind::Command,
                None,
            ))
            .await
        })
    };
    // The start subscribes and queues its read of the state in one poll,
    // and the store runs its operations in order, so what the caller writes
    // next comes after that read, which finds `held`.
    bounded(async {
        while manager.store.change_watchers() == watchers && !start.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    start
}

/// Start a command in `workspace` while an update holds it, then end the
/// update in `after`, returning the command's start.
async fn command_started_during_update(
    manager: &Arc<Manager>,
    workspace: &Workspace,
    after: WorkspaceState,
) -> Result<StartedExecution> {
    let start = command_started_during(manager, workspace, WorkspaceState::Updating).await;
    manager.set_state(&workspace.id, after, None).await.unwrap();
    bounded(start).await.unwrap()
}

#[tokio::test]
async fn a_command_started_during_an_update_runs_once_it_ends() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let started = command_started_during_update(&manager, &workspace, WorkspaceState::Ready)
        .await
        .unwrap();
    assert_eq!(started.space.id, workspace.id);
}

#[tokio::test]
async fn a_command_started_during_an_update_that_fails_is_refused() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let refused = command_started_during_update(&manager, &workspace, WorkspaceState::Failed)
        .await
        .unwrap_err();
    let reason = crate::refusal::Reason::of(&refused);
    assert_eq!(
        reason,
        Some(crate::refusal::Reason::NotReady),
        "{refused:#}"
    );
}

#[tokio::test]
async fn a_command_started_while_a_repair_has_its_worktree_away_runs_once_it_ends() {
    let (root, manager, workspace) = fixture("exit 0").await;
    // A reclamation holds the workspace while its worktree does not verify.
    let away = root.path().join("away");
    fs::rename(&workspace.path, &away).unwrap();
    let start = command_started_during(&manager, &workspace, WorkspaceState::Reconciling).await;
    fs::rename(&away, &workspace.path).unwrap();
    manager
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    let started = bounded(start).await.unwrap().unwrap();
    assert_eq!(started.space.id, workspace.id);
}

#[tokio::test]
async fn an_agent_registering_during_an_update_is_refused_instead_of_waiting() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
        .await
        .unwrap();
    // Its registration holds the queue's admission, which a stop needs.
    let refused = bounded(
        manager
            .agents
            .begin_agent_execution(&*manager, workspace.id.as_str()),
    )
    .await
    .unwrap_err();
    let reason = crate::refusal::Reason::of(&refused);
    assert_eq!(
        reason,
        Some(crate::refusal::Reason::NotReady),
        "{refused:#}"
    );
}

/// The error of a registration whose worktree did not verify.
fn unverified() -> anyhow::Error {
    anyhow::anyhow!("workspace directory is missing")
}

#[tokio::test]
async fn a_registration_failing_while_an_operation_that_passes_holds_it_waits() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let seen = Some(manager.lifecycle_generation(&workspace.id).await.unwrap());
    let error = unverified();
    let refused = || {
        manager
            .agents
            .reservation_refused(&*manager, &error, &workspace.id, seen)
    };
    assert!(!refused().await.unwrap());
    // As when a move reserves the workspace after the start's first look.
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
        .await
        .unwrap();
    assert!(refused().await.unwrap());
}

#[tokio::test]
async fn a_registration_failing_after_an_operation_came_and_went_retries() {
    let (_root, manager, workspace) = fixture("exit 0").await;
    let seen = manager.lifecycle_generation(&workspace.id).await.unwrap();
    // As when a checkpoint restore begins after the start's first look and
    // returns the workspace to the same state before its registration fails.
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Restoring)
        .await
        .unwrap();
    manager
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    assert!(
        (manager
            .agents
            .reservation_refused(&*manager, &unverified(), &workspace.id, Some(seen)))
        .await
        .unwrap()
    );
}

mod stopping;
