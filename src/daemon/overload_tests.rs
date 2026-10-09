use super::notifications::NotificationKind;
use super::*;
use crate::test_support::{manager, repository};
use std::{future::Future, os::unix::fs::PermissionsExt};

/// A hang guard, not a deadline for process scans on a shared CI host. Its
/// panic names the caller, so a hang shows which exchange never finished.
#[track_caller]
fn bounded<T>(future: impl Future<Output = T>) -> impl Future<Output = T> {
    let caller = std::panic::Location::caller();
    async move {
        timeout(Duration::from_secs(60), future)
            .await
            .unwrap_or_else(|_| panic!("overload exchange at {caller} timed out"))
    }
}

// Native ownership scans cover the whole process table. Another fixture's
// terminating process may have an unreadable environment before it disappears.
static PROCESS_FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct FixtureRoot {
    _root: tempfile::TempDir,
    _process_guard: tokio::sync::MutexGuard<'static, ()>,
}

async fn fixture() -> (
    FixtureRoot,
    Arc<Manager>,
    crate::model::Workspace,
    tokio::task::JoinHandle<()>,
) {
    let process_guard = PROCESS_FIXTURE.lock().await;
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
                    let operation = server.manager.background_operations.clone().read_owned().await;
                    clients.spawn(async move { serve(stream, server, operation).await });
                }
                Some(result) = clients.join_next(), if !clients.is_empty() => {
                    if let Err(error) = result.unwrap() {
                        eprintln!("fixture server failed: {error:#}");
                    }
                }
            }
        }
    });
    (
        FixtureRoot {
            _root: root,
            _process_guard: process_guard,
        },
        manager,
        workspace,
        serving,
    )
}

#[tokio::test]
async fn built_in_agents_resume_without_configuration_or_a_session_picker() {
    let root = tempfile::tempdir().unwrap();
    let mut command = crate::test_support::isolated_test(
        root.path(),
        "daemon::overload_tests::built_in_restore_child",
    );
    assert!(bounded(command.status()).await.unwrap().success());
}

#[tokio::test]
#[ignore = "isolated recovery lookup launched by built-in restore test"]
async fn built_in_restore_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    use crate::execution::recovery::Recovery;
    let (_root, manager, workspace, serving) = fixture().await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[commands]\ncodex = ['codex-fixture', '{args}']\nclaude = ['claude-fixture', '{args}']\n",
    )
    .unwrap();
    for (agent, handoff, expected) in [
        ("codex", None, vec!["codex-fixture", "resume", "--last"]),
        ("claude", None, vec!["claude-fixture", "--continue"]),
        (
            "codex",
            Some("handoff"),
            vec!["codex-fixture", "resume", "--last", "handoff"],
        ),
        (
            "claude",
            Some("handoff"),
            vec!["claude-fixture", "--continue", "handoff"],
        ),
    ] {
        let recovery = Recovery::resolve(&manager.paths, &workspace, agent, handoff)
            .await
            .unwrap();
        assert!(recovery.automatic);
        assert_eq!(recovery.handoff_delivered, handoff.is_some());
        assert_eq!(
            recovery.command,
            expected
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        );
    }
    let unsupported = Recovery::resolve(&manager.paths, &workspace, "custom", None)
        .await
        .unwrap();
    assert!(!unsupported.automatic);
    assert!(unsupported.command.is_empty());
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\ncodex = ['saved-session', 'specific-id']\n",
    )
    .unwrap();
    let overridden = Recovery::resolve(&manager.paths, &workspace, "codex", Some("handoff"))
        .await
        .unwrap();
    assert!(overridden.automatic);
    assert!(!overridden.handoff_delivered);
    assert_eq!(
        overridden.command,
        ["saved-session", "specific-id"].map(std::ffi::OsString::from)
    );
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\ncodex = ['saved-session', '--', '{prompt}']\n",
    )
    .unwrap();
    let prompted = Recovery::resolve(&manager.paths, &workspace, "codex", Some("handoff"))
        .await
        .unwrap();
    assert!(prompted.handoff_delivered);
    assert_eq!(
        prompted.command,
        ["saved-session", "--", "handoff"].map(std::ffi::OsString::from)
    );
    serving.abort();
}

#[track_caller]
fn wait_started<'a>(
    manager: &'a Manager,
    workspace: &'a crate::model::Workspace,
) -> impl Future<Output = ()> + 'a {
    wait_launched(manager, workspace, "started", 1)
}

/// Waits until the child has written `marker`, so its TERM trap is set, and
/// the daemon has recorded the process groups of `executions` executions. The
/// marker alone can appear before the wrapper's start report is recorded, and
/// a stop sent then reaches an execution that never paused and whose launch
/// is unproven.
#[track_caller]
fn wait_launched<'a>(
    manager: &'a Manager,
    workspace: &'a crate::model::Workspace,
    marker: &'a str,
    executions: usize,
) -> impl Future<Output = ()> + 'a {
    bounded(async move {
        loop {
            let id = workspace.id.clone();
            let recorded = manager
                .store
                .run(move |db| {
                    Ok(store::executions(db, &id)?
                        .iter()
                        .filter(|record| record.group_id.is_some())
                        .count())
                })
                .await
                .unwrap();
            if workspace.path.join(marker).is_file() && recorded >= executions {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// A pause clears the process group that `wait_started` saw recorded.
#[track_caller]
fn wait_paused<'a>(
    manager: &'a Manager,
    workspace: &'a crate::model::Workspace,
) -> impl Future<Output = ()> + 'a {
    bounded(async move {
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
}

fn launch(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> tokio::task::JoinHandle<Result<i32>> {
    launch_child(
        manager,
        workspace,
        "daemon::overload_tests::tracked_agent_child",
    )
}

fn launch_child(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> tokio::task::JoinHandle<Result<i32>> {
    let mut command = crate::test_support::isolated_test(&manager.paths.home, test);
    command.env("SHOAL_TEST_WORKSPACE", &workspace.id);
    tokio::spawn(async move {
        let status = command.status().await?;
        Ok(crate::execution::exit_code(status))
    })
}

#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let code = crate::execution::run(
        &paths,
        id,
        vec![
            "sh".into(),
            "-c".into(),
            "trap 'exit 0' TERM; touch started; while :; do sleep 1; done".into(),
        ],
        Some("fixture".into()),
    )
    .await
    .unwrap();
    std::process::exit(code);
}

#[tokio::test]
#[ignore = "isolated tracked command launched by the stop test"]
async fn tracked_command_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let code = crate::execution::run_command(
        &paths,
        id,
        vec![
            "sh".into(),
            "-c".into(),
            "trap 'exit 0' TERM; touch command-started; while :; do sleep 1; done".into(),
        ],
    )
    .await
    .unwrap();
    std::process::exit(code);
}

#[tokio::test]
async fn stop_saves_agents_and_commands_preserving_work_and_leases() {
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
    fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    let ctx = crate::cli::context::Context::new(manager.paths.clone(), true);
    crate::cli::client::request::<()>(
        &manager.paths,
        Method::StopWorkspace {
            workspace: workspace.id.clone(),
        },
    )
    .await
    .unwrap();
    bounded(agent).await.unwrap().unwrap();
    bounded(command).await.unwrap().unwrap();
    let retained = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(retained.workspace.state, workspace.state);
    assert_eq!(retained.ports.len(), 1);
    assert!(retained.executions.is_empty());
    assert_eq!(
        fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    let commands = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".command.json"))
        .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
        .collect::<Vec<serde_json::Value>>();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["argv"][0], "sh");
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    // A manual stop never restores automatically.
    manager.recovery_ready.send_replace(Some(0));
    assert!(!workspace.path.join("restored").exists());
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf '%s' \"$1\" > restored\n",
    )
    .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\nfixture = ['./resume.sh', '{prompt}']\n",
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
    let handoff = fs::read_to_string(workspace.path.join("restored")).unwrap();
    assert!(handoff.contains("\n- sh -c 'trap"), "{handoff}");
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn resume_without_an_agent_reports_stopped_commands_once() {
    let (_root, manager, workspace, serving) = fixture().await;
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_launched(&manager, &workspace, "command-started", 1).await;
    bounded(manager.stop_workspace(&workspace.id, StopRecords::Save))
        .await
        .unwrap();
    bounded(command).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let ctx = crate::cli::context::Context::new(manager.paths.clone(), true);
    let resume =
        || crate::cli::commands::resume::run(&ctx, Some(workspace.id.clone()), None, false);
    assert_eq!(bounded(resume()).await.unwrap(), 0);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(bounded(resume()).await.is_err());
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
    wait_started(&manager, &workspace).await;
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
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&workspace.id, StopRecords::Save))
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
    assert_eq!(
        record,
        serde_json::json!({"agent": "fixture", "stop_reason": "test memory pressure"})
    );
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
async fn shutdown_finishes_an_agent_waiting_for_overload_recovery() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    // The stop watch already holds `true`; sending it again must still wake
    // the waiting recovery so the wrapper finishes before shutdown completes.
    bounded(manager.stop_for_shutdown()).await;
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
    wait_started(&manager, &workspace).await;
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
    wait_started(&manager, &workspace).await;
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
    bounded(manager.stop_workspace(&workspace.id, StopRecords::Save))
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
    wait_started(&manager, &workspace).await;
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

async fn set_recovery(manager: &Manager, enabled: bool) {
    let config = crate::config::Config::path(&manager.paths);
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        config,
        format!("[overload.recovery]\nenabled = {enabled}\n"),
    )
    .unwrap();
    manager.reload_config().await.unwrap();
}

fn publish_recovery(manager: &Manager) {
    let epoch = manager
        .recovery_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    manager.recovery_ready.send_replace(Some(epoch));
}

#[tokio::test]
async fn disabling_recovery_while_an_agent_waits_finishes_it_with_a_restore_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(manager.stop_agent_for_overload("test pressure").await);
    wait_paused(&manager, &workspace).await;
    // Load never recovers here: disabling alone must end the wait.
    set_recovery(&manager, false).await;
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn enabling_recovery_applies_to_an_agent_started_without_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    set_recovery(&manager, false).await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    set_recovery(&manager, true).await;
    assert!(manager.stop_agent_for_overload("test pressure").await);
    wait_paused(&manager, &workspace).await;
    publish_recovery(&manager);
    bounded(launched).await.unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

#[tokio::test]
async fn recovery_disconnect_closes_the_connection_without_forgetting_ownership() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
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
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&workspace.id, StopRecords::Save))
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

#[tokio::test]
async fn critical_disk_space_stops_executions_cleanup_cannot_remove() {
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    bounded(super::disk::Monitor::default().check(
        &manager,
        tokio::time::Instant::now(),
        |_: &std::path::Path| Ok(0),
    ))
    .await
    .unwrap();
    bounded(agent).await.unwrap().unwrap();
    let retained = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert!(retained.executions.is_empty());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let stopped = manager
        .notifications(false, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == NotificationKind::AgentStopped)
        .unwrap();
    assert!(
        stopped.message.contains("shoal resume load"),
        "{}",
        stopped.message
    );
    serving.abort();
}
