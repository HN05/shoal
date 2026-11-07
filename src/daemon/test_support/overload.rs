//! The overload tests' fixture: a manager serving its socket, with a
//! workspace whose agent restores through a configured command, and the
//! tracked agents and commands the tests start in it.
use std::{
    fs::{self, OpenOptions},
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::{net::UnixListener, task::JoinSet, time::timeout};

use super::{install_test_script, isolated_test, manager, repository};
use crate::client::space_names::SpaceSelector;
use crate::daemon::{Server, connections, serve, store, workspace::Manager};

/// A hang guard, not a deadline for process scans on a shared CI host. Its
/// panic names the caller, so a hang shows which exchange never finished.
#[track_caller]
pub(crate) fn bounded<T>(future: impl Future<Output = T>) -> impl Future<Output = T> {
    let caller = std::panic::Location::caller();
    async move {
        timeout(Duration::from_secs(60), future)
            .await
            .unwrap_or_else(|_| panic!("overload exchange at {caller} timed out"))
    }
}

// Native ownership scans cover the whole process table. Another fixture's
// terminating process may have an unreadable environment before it disappears.
pub(crate) static PROCESS_FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct FixtureRoot {
    _root: tempfile::TempDir,
    _process_guard: tokio::sync::MutexGuard<'static, ()>,
    _process_file_guard: std::fs::File,
}

pub(crate) async fn fixture() -> (
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
    install_test_script(&script, "#!/bin/sh\nprintf restored > restored\n");
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./resume.sh']\n",
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
pub(crate) fn serve_fixture(manager: Arc<Manager>) -> tokio::task::JoinHandle<()> {
    let _ = fs::remove_file(&manager.paths.socket);
    let listener = UnixListener::bind(&manager.paths.socket).unwrap();
    let server = Server {
        manager,
        started: Instant::now(),
        managed: false,
        exit: tokio::sync::mpsc::channel(1).0,
        capacity: Arc::new(connections::Capacity::new(128, 128)),
    };
    tokio::spawn(async move {
        let mut clients = JoinSet::new();
        // Restores wait in the agent queue; aborting the server ends it too.
        let manager = server.manager.clone();
        clients.spawn(async move {
            crate::daemon::queued_starts::run(manager).await;
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

#[track_caller]
pub(crate) fn wait_started<'a>(
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
pub(crate) fn wait_launched<'a>(
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
pub(crate) fn wait_paused<'a>(
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

pub(crate) fn launch(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> tokio::task::JoinHandle<Result<i32>> {
    launch_child(
        manager,
        workspace,
        "daemon::overload_tests::tracked_agent_child",
    )
}

pub(crate) fn launch_child(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> tokio::task::JoinHandle<Result<i32>> {
    run_child(child_command(manager, workspace, test))
}

/// The agent helper `test` as a wrapper in `workspace`.
pub(crate) fn child_command(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> tokio::process::Command {
    let mut command = isolated_test(&manager.paths.home, test);
    command.env("SHOAL_TEST_WORKSPACE", &workspace.id);
    command
}

pub(crate) fn run_child(
    mut command: tokio::process::Command,
) -> tokio::task::JoinHandle<Result<i32>> {
    tokio::spawn(async move {
        let status = command.status().await?;
        Ok(crate::execution::exit_code(status))
    })
}

/// Report `session` from the workspace's running agent, as its hooks do.
pub(crate) async fn report_session(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    session: &str,
) {
    let execution = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(
            &SpaceSelector::id(&workspace.id),
            crate::state::AgentState::Idle,
            Some(execution),
            Some(session.into()),
        )
        .await
        .unwrap();
}
