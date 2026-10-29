use super::notifications::NotificationKind;
use super::*;
use crate::{
    daemon::workspace::{ExecutionKind, StopRequest},
    protocol::RemovalMethod,
    test_support::{manager, repository},
};
use std::{
    fs::{self, OpenOptions},
    future::Future,
    os::unix::fs::PermissionsExt,
};
use tokio::net::UnixListener;

mod agent_restart;
mod idle_cleanup;
mod queue_move;
mod restart;

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
    _process_file_guard: std::fs::File,
}

async fn fixture() -> (
    FixtureRoot,
    Arc<Manager>,
    crate::model::Workspace,
    tokio::task::JoinHandle<()>,
) {
    let process_guard = PROCESS_FIXTURE.lock().await;
    let process_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(std::env::temp_dir().join("shoal-overload-tests.lock"))
        .unwrap();
    let process_file_guard = tokio::task::spawn_blocking({
        let process_file = process_file;
        move || {
            fs2::FileExt::lock_exclusive(&process_file).unwrap();
            process_file
        }
    })
    .await
    .unwrap();
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
    let serving = serve_fixture(manager.clone());
    (
        FixtureRoot {
            _root: root,
            _process_guard: process_guard,
            _process_file_guard: process_file_guard,
        },
        manager,
        workspace,
        serving,
    )
}

/// Serve `manager` on its socket as the daemon does, replacing a socket left
/// by an earlier fixture server.
fn serve_fixture(manager: Arc<Manager>) -> tokio::task::JoinHandle<()> {
    let _ = fs::remove_file(&manager.paths.socket);
    let listener = UnixListener::bind(&manager.paths.socket).unwrap();
    let server = Server {
        manager,
        started: Instant::now(),
        managed: false,
        shutdown: watch::channel(false).0,
        capacity: Arc::new(connections::Capacity::new(128, 128)),
    };
    tokio::spawn(async move {
        let mut clients = JoinSet::new();
        // Restores wait in the agent queue; aborting the server ends it too.
        let manager = server.manager.clone();
        clients.spawn(async move {
            crate::daemon::agent_queue::run(manager).await;
            Ok(())
        });
        loop {
            tokio::select! {
                accepted = connections::accept(&listener) => {
                    let stream = accepted.unwrap();
                    let server = server.clone();
                    let operation = server.manager.gates.owned_operation().await;
                    clients.spawn(async move { serve(stream, server, operation).await });
                }
                Some(result) = clients.join_next(), if !clients.is_empty() => {
                    if let Err(error) = result.unwrap() {
                        eprintln!("fixture server failed: {error:#}");
                    }
                }
            }
        }
    })
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
    let codex = [
        "codex-fixture",
        "resume",
        "--last",
        "-c",
        "features.worktrees=false",
    ];
    for (agent, handoff, restore) in [
        ("codex", None, &codex[..]),
        ("claude", None, &["claude-fixture", "--continue"][..]),
        ("codex", Some("handoff"), &codex[..]),
        (
            "claude",
            Some("handoff"),
            &["claude-fixture", "--continue"][..],
        ),
    ] {
        let recovery = Recovery::resolve(&manager.paths, &workspace, agent, handoff, None)
            .await
            .unwrap();
        assert!(recovery.automatic);
        assert_eq!(recovery.handoff_delivered, handoff.is_some());
        // Restored sessions keep receiving agent messages and may work in
        // the child workspaces beside theirs.
        let directory = workspace.path.parent().unwrap();
        let tools = crate::ai::Table::default().tools;
        let tool = &tools[agent];
        let launch_args = crate::cli::agents::hook_args(tool.integration, false)
            .unwrap()
            .into_iter()
            .chain(tool.launch.arguments.add_dir_args(directory));
        let mut prompt = String::from(
            "Shoal resumed this session. Continue working on the unfinished assignment from where you left off.",
        );
        if let Some(handoff) = handoff {
            prompt.push_str("\n\n");
            prompt.push_str(handoff);
        }
        let expected: Vec<std::ffi::OsString> = restore
            .iter()
            .map(std::ffi::OsString::from)
            .chain(launch_args)
            .chain([std::ffi::OsString::from(prompt)])
            .collect();
        assert_eq!(recovery.command, expected);
    }
    // A reported session is resumed by its ID.
    for (agent, restore) in [
        ("codex", &["codex-fixture", "resume", "s1"][..]),
        ("claude", &["claude-fixture", "--resume", "s1"][..]),
    ] {
        let recovery = Recovery::resolve(&manager.paths, &workspace, agent, None, Some("s1"))
            .await
            .unwrap();
        let restore = restore.iter().map(std::ffi::OsString::from);
        assert!(recovery.command.starts_with(&restore.collect::<Vec<_>>()));
    }
    let unsupported = Recovery::resolve(&manager.paths, &workspace, "custom", None, None)
        .await
        .unwrap();
    assert!(!unsupported.automatic);
    assert!(unsupported.command.is_empty());
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agent_resume]\ncodex = ['saved-session', 'specific-id']\n",
    )
    .unwrap();
    let overridden = Recovery::resolve(&manager.paths, &workspace, "codex", Some("handoff"), None)
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
    let prompted = Recovery::resolve(&manager.paths, &workspace, "codex", Some("handoff"), None)
        .await
        .unwrap();
    assert!(prompted.handoff_delivered);
    assert_eq!(
        prompted.command,
        [
            "saved-session",
            "--",
            "Shoal resumed this session. Continue working on the unfinished assignment from where you left off.\n\nhandoff",
        ]
        .map(std::ffi::OsString::from)
    );
    let prompted = Recovery::resolve(&manager.paths, &workspace, "codex", None, None)
        .await
        .unwrap();
    assert!(!prompted.handoff_delivered);
    assert_eq!(
        prompted.command,
        [
            "saved-session",
            "--",
            "Shoal resumed this session. Continue working on the unfinished assignment from where you left off.",
        ]
        .map(std::ffi::OsString::from)
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

/// Waits until the stopped agent's restore waits in the agent queue.
#[track_caller]
fn wait_restore_queued(manager: &Manager) -> impl Future<Output = ()> + '_ {
    bounded(async move {
        // Subscribe before the first check so a concurrent commit cannot be missed.
        let mut changed = manager.store.watch_changes();
        while !manager
            .queue_entries()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.kind == crate::state::QueueKind::Restore)
        {
            changed.changed().await.unwrap();
        }
    })
}

async fn queue_is_empty(manager: &Manager) -> bool {
    manager.queue_entries().await.unwrap().is_empty()
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

fn isolated_operation(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> tokio::process::Command {
    let mut command = crate::test_support::isolated_test(&manager.paths.home, test);
    command
        .env("SHOAL_TEST_ROOT", &manager.paths.home)
        .env("SHOAL_TEST_WORKSPACE", &workspace.id);
    command
}

fn resume_isolated(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    discard: bool,
) -> tokio::task::JoinHandle<Result<i32>> {
    let mut command =
        isolated_operation(manager, workspace, "daemon::overload_tests::resume_child");
    command.env("SHOAL_TEST_DISCARD", discard.to_string());
    tokio::spawn(async move {
        let status = command.status().await?;
        Ok(crate::execution::exit_code(status))
    })
}

async fn stop_isolated(manager: &Manager, workspace: &crate::model::Workspace) -> Result<i32> {
    let mut command = isolated_operation(manager, workspace, "daemon::overload_tests::stop_child");
    let status = command.status().await?;
    Ok(crate::execution::exit_code(status))
}

#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent("trap 'exit 0' TERM; touch started; while :; do sleep 1; done").await;
}

/// An agent that keeps running when stopped. Once the stop reaches it, it
/// suspends its parent, the wrapper, and writes `terminated`, so the wrapper
/// stays where the stop reached the agent.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_holding_on() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(
        "trap 'kill -STOP $PPID; touch terminated' TERM; touch started; while :; do sleep 1; done",
    )
    .await;
}

/// Like `tracked_agent_child`, with a background command in its own process
/// group, as Claude Code runs shell commands. It writes its PID to `background`
/// and keeps running in Python: macOS hides platform binaries' environments.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_with_background_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(&with_background("touch started; while :; do sleep 1; done")).await;
}

/// Like `tracked_agent_with_background_child`, but the agent exits on its own
/// once the test creates `exit`, leaving its background command running.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_exiting_with_background_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(&with_background(
        "touch started; while [ ! -e exit ]; do sleep 1; done",
    ))
    .await;
}

/// An agent that exits on its own once the test creates `exit`.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_exiting_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent("trap 'exit 0' TERM; touch started; while [ ! -e exit ]; do sleep 1; done")
        .await;
}

/// Run `script` after starting a background command in its own process group.
fn with_background(script: &str) -> String {
    concat!(
        "trap 'exit 0' TERM; python3 -c \"",
        "import os, time; os.setpgid(0, 0); ",
        "open('background.tmp', 'w').write(str(os.getpid())); ",
        "os.rename('background.tmp', 'background'); ",
        "time.sleep(300)\" & ",
    )
    .to_owned()
        + script
}

async fn run_tracked_agent(script: &str) {
    let root = std::env::current_dir().unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let code = crate::execution::run(
        &paths,
        id,
        vec!["sh".into(), "-c".into(), script.into()],
        Some("fixture".into()),
    )
    .await
    // A daemon crash ends the wrapper with an error, as it does a real one.
    .unwrap_or_else(|error| {
        eprintln!("shoal: {error:#}");
        1
    });
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

// These cases call CLI handlers in-process. Run them in an env-cleared child
// so a tracked test runner cannot lend its scope to the fixture daemon.
async fn run_isolated_overload_test(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut command = crate::test_support::isolated_test(
        root.path(),
        "daemon::overload_tests::isolated_overload_test",
    );
    command.env("SHOAL_OVERLOAD_TEST", name);
    assert!(bounded(command.status()).await.unwrap().success());
}

#[tokio::test]
#[ignore = "isolated overload test invoked by its in-process wrapper"]
async fn isolated_overload_test() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    match std::env::var("SHOAL_OVERLOAD_TEST").unwrap().as_str() {
        "stop_saves_agents_and_commands_preserving_work_and_leases" => {
            stop_saves_agents_and_commands_preserving_work_and_leases_body().await
        }
        "resume_without_an_agent_reports_stopped_commands_once" => {
            resume_without_an_agent_reports_stopped_commands_once_body().await
        }
        "a_restored_stopped_agent_is_told_the_commands_stopped_with_it" => {
            a_restored_stopped_agent_is_told_the_commands_stopped_with_it_body().await
        }
        "resume_continues_an_agent_that_exited_on_its_own" => {
            resume_continues_an_agent_that_exited_on_its_own_body().await
        }
        "discard_clears_pending_recovery_without_launching_an_agent" => {
            discard_clears_pending_recovery_without_launching_an_agent_body().await
        }
        "manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails" => {
            manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails_body().await
        }
        name => panic!("unknown isolated overload test: {name}"),
    }
}

#[tokio::test]
#[ignore = "isolated stop operation launched by overload tests"]
async fn stop_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::var_os("SHOAL_TEST_ROOT").unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    crate::cli::client::request::<()>(
        &paths,
        RemovalMethod::StopWorkspace {
            workspace: id,
            acknowledge_unproven: false,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "isolated resume operation launched by overload tests"]
async fn resume_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::var_os("SHOAL_TEST_ROOT").unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let discard = std::env::var("SHOAL_TEST_DISCARD").unwrap() == "true";
    let ctx = crate::cli::context::Context::new(paths, true);
    match crate::cli::commands::resume::run(&ctx, id, None, discard, true).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}

#[tokio::test]
async fn stop_saves_agents_and_commands_preserving_work_and_leases() {
    run_isolated_overload_test("stop_saves_agents_and_commands_preserving_work_and_leases").await;
}

async fn stop_saves_agents_and_commands_preserving_work_and_leases_body() {
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
    assert_eq!(
        bounded(stop_isolated(&manager, &workspace)).await.unwrap(),
        0
    );
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
    manager.overload_recovery.publish(Some(0));
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
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let handoff = fs::read_to_string(workspace.path.join("restored")).unwrap();
    assert!(handoff.contains("\n- sh -c 'trap"), "{handoff}");
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn a_restored_stopped_agent_is_told_the_commands_stopped_with_it() {
    run_isolated_overload_test("a_restored_stopped_agent_is_told_the_commands_stopped_with_it")
        .await;
}

async fn a_restored_stopped_agent_is_told_the_commands_stopped_with_it_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    // A stop other than `shoal stop`, such as a daemon shutdown's, keeps the
    // agent as stopped work.
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    bounded(agent).await.unwrap().unwrap();
    bounded(command).await.unwrap().unwrap();
    let saved = crate::execution::recovery::Saved::load(&manager.paths, &workspace.id).unwrap();
    assert_eq!((saved.agents.len(), saved.commands.len()), (1, 1));
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
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
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
    run_isolated_overload_test("resume_without_an_agent_reports_stopped_commands_once").await;
}

async fn resume_without_an_agent_reports_stopped_commands_once_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_launched(&manager, &workspace, "command-started", 1).await;
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    bounded(command).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let resume = || resume_isolated(&manager, &workspace, false);
    assert_eq!(bounded(resume()).await.unwrap().unwrap(), 0);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_ne!(bounded(resume()).await.unwrap().unwrap(), 0);
    serving.abort();
}

#[tokio::test]
async fn resume_continues_an_agent_that_exited_on_its_own() {
    run_isolated_overload_test("resume_continues_an_agent_that_exited_on_its_own").await;
}

/// Report `session` from the workspace's running agent, as its hooks do.
async fn report_session(manager: &Manager, workspace: &crate::model::Workspace, session: &str) {
    let execution = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(
            &workspace.id,
            crate::state::AgentState::Idle,
            Some(execution),
            Some(session.into()),
        )
        .await
        .unwrap();
}

async fn resume_continues_an_agent_that_exited_on_its_own_body() {
    use crate::execution::recovery::{LastSession, Saved};
    let (_root, manager, workspace, serving) = fixture().await;
    let last = || LastSession::all(&manager.paths, &workspace.id).unwrap();
    // An agent `shoal stop` stops leaves its session as one that exits on its
    // own does, which uses no slot and keeps nothing from cleanup.
    let agent = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "stopped-session").await;
    assert_eq!(
        bounded(stop_isolated(&manager, &workspace)).await.unwrap(),
        0
    );
    bounded(agent).await.unwrap().unwrap();
    let [stopped] = last().try_into().unwrap();
    assert_eq!(stopped.session.as_deref(), Some("stopped-session"));
    assert!(
        Saved::load(&manager.paths, &workspace.id)
            .unwrap()
            .agents
            .is_empty()
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());

    // A new agent replaces it, and leaves its own session when it exits on
    // its own.
    fs::remove_file(workspace.path.join("started")).unwrap();
    let agent = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_exiting_child",
    );
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "quit-session").await;
    fs::write(workspace.path.join("exit"), "").unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 0);
    let [quit] = last().try_into().unwrap();
    assert_eq!(
        (quit.agent.as_str(), quit.session.as_deref(), quit.moved),
        ("fixture", Some("quit-session"), false)
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    // The restored agent exited on its own too; finishing the assignment
    // forgets it.
    assert!(!last().is_empty());
    manager
        .mark_done(&workspace.id, false, false)
        .await
        .unwrap();
    assert_eq!(last(), []);
    assert_ne!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    serving.abort();
}

#[tokio::test]
async fn an_agent_whose_terminal_closes_is_saved_as_stopped() {
    use crate::execution::recovery::{LastSession, Record, Saved};
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "closed-session").await;
    let wrapper = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .wrapper
        .clone()
        .unwrap();
    crate::process::identity::signal(&wrapper, libc::SIGHUP).unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 129);
    assert_eq!(LastSession::all(&manager.paths, &workspace.id).unwrap(), []);
    let saved = Saved::load(&manager.paths, &workspace.id).unwrap();
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    assert_eq!(record.stop_reason.as_deref(), Some("its terminal closed"));
    assert_eq!(record.session.as_deref(), Some("closed-session"));
    serving.abort();
}

/// A shutdown terminates the wrapper and may kill it before its agent exits,
/// so the record is saved before the agent is stopped.
#[tokio::test]
async fn an_agent_whose_session_ends_is_saved_before_it_stops() {
    use crate::execution::recovery::{Record, record_path, restore_requests};
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_holding_on",
    );
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "ending-session").await;
    let execution = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions
        .remove(0);
    // The daemon keeps the agent's name, which its session needs without a record.
    let id = execution.id.clone();
    let agent_name = manager
        .store
        .run(move |db| store::execution_agent(db, &id))
        .await
        .unwrap();
    assert_eq!(agent_name.as_deref(), Some("fixture"));
    let wrapper = execution.wrapper.clone().unwrap();
    crate::process::identity::signal(&wrapper, libc::SIGTERM).unwrap();
    // The agent traps the stop the wrapper sends it, which follows the save,
    // and suspends the wrapper before it could save once the agent stopped.
    bounded(async {
        while !workspace.path.join("terminated").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    crate::process::identity::signal(&wrapper, libc::SIGKILL).unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 128 + libc::SIGKILL);
    // SAFETY: the killed wrapper's agent group, which nothing else stops.
    unsafe {
        libc::kill(-(execution.group_id.unwrap() as i32), libc::SIGKILL);
    }
    let path = record_path(&manager.paths, &workspace.id, &execution.id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.agent, "fixture");
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("its session was terminated")
    );
    assert_eq!(record.session.as_deref(), Some("ending-session"));
    // It comes back through the agent queue once its execution has ended.
    let requested: Vec<_> = restore_requests(&manager.paths, &workspace.id)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(requested, [execution.id]);
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
    let monitor = manager.gates.operation().await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    drop(monitor);
    // An update waits for the restore, so the stop holds one back.
    assert!(manager.gates.try_quiesce().is_none());
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    assert!(manager.gates.try_quiesce().is_none());
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 7);
    // The restored agent took over the slot its restore kept.
    assert!(queue_is_empty(&manager).await);
    drop(bounded(manager.gates.quiesce()).await);
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
async fn an_idle_agent_waiting_for_an_update_lets_a_daemon_update_through() {
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
        .set_agent_state(
            &workspace.name,
            crate::state::AgentState::Idle,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    let monitor = manager.gates.operation().await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    drop(monitor);
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    // Waiting for an update lets go of the hold once its processes stopped.
    bounded(async {
        while manager.gates.try_quiesce().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(!launched.is_finished());
    // The fixture runs no disk monitor; a reading finds the space free.
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    manager
        .send_agent_message(&workspace.name, "Review requested".into())
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .any(|event| event.message.ends_with("for an update"))
    );
    serving.abort();
}

/// Launch an agent with a background command outside its process group and
/// return that command's identity once both run.
async fn launch_with_background(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> (
    tokio::task::JoinHandle<Result<i32>>,
    crate::process::identity::Identity,
) {
    launch_agent_with_background(
        manager,
        workspace,
        "daemon::overload_tests::tracked_agent_with_background_child",
    )
    .await
}

/// Launch the agent helper `test`, which starts a background command outside
/// its process group, and return that command's identity once both run.
async fn launch_agent_with_background(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> (
    tokio::task::JoinHandle<Result<i32>>,
    crate::process::identity::Identity,
) {
    let launched = launch_child(manager, workspace, test);
    wait_launched(manager, workspace, "background", 1).await;
    wait_started(manager, workspace).await;
    let pid = fs::read_to_string(workspace.path.join("background")).unwrap();
    let background = crate::process::identity::capture(pid.parse().unwrap())
        .unwrap()
        .unwrap();
    (launched, background)
}

#[tokio::test]
async fn agent_exit_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_agent_with_background(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_exiting_with_background_child",
    )
    .await;
    fs::write(workspace.path.join("exit"), "").unwrap();
    // The wrapper exits only after the daemon acknowledges the exit.
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert!(!crate::process::identity::alive(&background).unwrap());
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
async fn overload_stops_commands_outside_the_agent_group_and_restores_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert!(!crate::process::identity::alive(&background).unwrap());
    publish_recovery(&manager);
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

#[tokio::test]
async fn overload_without_recovery_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    set_recovery(&manager, false).await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!crate::process::identity::alive(&background).unwrap());
    // The execution ended proven, so `shoal resume` can select its record.
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn cancelling_a_restore_still_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    // Disabling recovery cancels the restore however far the pause has come.
    set_recovery(&manager, false).await;
    bounded(launched).await.unwrap().unwrap();
    assert!(!crate::process::identity::alive(&background).unwrap());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

/// Set the recorded process group of the workspace's only execution.
async fn record_group(manager: &Manager, workspace: &crate::model::Workspace, group: Option<u32>) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET group_id=?2 WHERE workspace_id=?1",
                rusqlite::params![id, group],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unproven_stop_keeps_the_agent_waiting_until_proof_then_restores_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    // An incomplete launch identity stands in for survivors Shoal cannot stop.
    let group = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .group_id;
    record_group(&manager, &workspace, None).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    let message = bounded(async {
        loop {
            if let Some(event) = manager
                .notifications(false, 10)
                .await
                .unwrap()
                .into_iter()
                .find(|event| event.kind == NotificationKind::RecoveryBlocked)
            {
                break event.message;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        message.contains(
            "waits until its remaining processes exit: its launch identity is incomplete"
        ),
        "{message}"
    );
    publish_recovery(&manager);
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    record_group(&manager, &workspace, group).await;
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
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
    wait_restore_queued(&manager).await;
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    let records = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let text = fs::read_to_string(&records[0]).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&text).unwrap();
    // The wrapper that waited has exited, so resume restores the agent.
    let waiting = record.as_object_mut().unwrap().remove("waiting").unwrap();
    let waiting: crate::process::identity::Identity = serde_json::from_value(waiting).unwrap();
    assert!(!crate::process::identity::alive(&waiting).unwrap());
    assert!(crate::execution::recovery::any_pending(&manager.paths));
    assert_eq!(
        record,
        serde_json::json!({
            "agent": "fixture",
            "stop_reason": "test memory pressure",
            "stage": "stopped",
        })
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
    run_isolated_overload_test("discard_clears_pending_recovery_without_launching_an_agent").await;
}

async fn discard_clears_pending_recovery_without_launching_an_agent_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert_ne!(
        bounded(resume_isolated(&manager, &workspace, true))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
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
        bounded(resume_isolated(&manager, &workspace, true))
            .await
            .unwrap()
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
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
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
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .all(|event| !event
                .message
                .contains("recovery handoff could not be saved"))
    );
    serving.abort();
}

#[tokio::test]
async fn a_done_workspace_does_not_restore_its_stopped_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    manager
        .mark_done(&workspace.id, false, false)
        .await
        .unwrap();
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn finishing_the_assignment_cancels_a_waiting_restore() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    manager
        .mark_done(&workspace.id, false, false)
        .await
        .unwrap();
    publish_recovery(&manager);
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

async fn set_recovery(manager: &Manager, enabled: bool) {
    crate::test_support::configure(
        manager,
        &format!("[overload.recovery]\nenabled = {enabled}\n"),
    )
    .await;
}

fn publish_recovery(manager: &Manager) {
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
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
    wait_restore_queued(&manager).await;
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
    // The restore keeps its place while the wrapper may reattach.
    assert!(!queue_is_empty(&manager).await);
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
    run_isolated_overload_test(
        "manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails",
    )
    .await;
}

async fn manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails_body()
 {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
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
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
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
async fn critical_disk_space_stops_commands_and_restores_agents_once_space_recovers() {
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    let mut monitor = super::disk::Monitor::default();
    let now = tokio::time::Instant::now();
    let empty = |_: &std::path::Path| Ok(0);
    bounded(monitor.check(&manager, now, empty)).await.unwrap();
    bounded(command).await.unwrap().unwrap();
    wait_paused(&manager, &workspace).await;
    let records = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&records[0]).unwrap()).unwrap();
    let reason = record["stop_reason"].as_str().unwrap();
    assert!(
        reason.starts_with("free disk space is below 2 GiB at ")
            && reason.ends_with("(0.0 GiB free); all running agents stopped"),
        "{reason}"
    );
    let messages = manager
        .notifications(false, 10)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == NotificationKind::AgentStopped)
        .map(|event| event.message)
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages.iter().any(|message| message
            .contains("automatic session restore waits until free disk space reaches 5 GiB")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("then shoal resume load lists them")),
        "{messages:?}"
    );
    // Later critical readings leave the waiting agent's restore in place.
    bounded(monitor.check(&manager, now, empty)).await.unwrap();
    assert!(!manager.config_state.disk_space_recovered());
    bounded(monitor.check(&manager, now, |_: &std::path::Path| Ok(u64::MAX)))
        .await
        .unwrap();
    assert!(manager.config_state.disk_space_recovered());
    publish_recovery(&manager);
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}
