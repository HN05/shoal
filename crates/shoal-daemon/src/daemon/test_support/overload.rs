//! The overload tests' fixture: a manager serving its socket, with a
//! workspace, and on request a chat, whose agents restore through a
//! configured command, and the tracked agents and commands the tests start
//! in them.
use std::{
    fs::{self, OpenOptions},
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{net::UnixListener, task::JoinSet, time::timeout};

use super::{install_test_script, manager, repository};
use crate::daemon::{
    Server, connections, manager::Manager, notifications::AlertKind, serve, store,
};
use crate::execution::recovery::{Record, RecordExt as _, record_path};
use crate::model::Execution;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// A hang guard, not a deadline for process scans on a shared CI host. Its
/// panic names the caller, so a hang shows which exchange never finished.
#[track_caller]
pub fn bounded<T>(future: impl Future<Output = T>) -> impl Future<Output = T> {
    let caller = std::panic::Location::caller();
    async move {
        timeout(Duration::from_secs(60), future)
            .await
            .unwrap_or_else(|_| panic!("overload exchange at {caller} timed out"))
    }
}

/// Replace the fixture's global config with `text`, keeping admission off
/// the host's readings: under the default measured limit, critical memory
/// or disk on the machine running the tests holds a restore back for as
/// long as it lasts, and under a fixed one, disk does.
pub async fn configure_fixture(manager: &Manager, text: &str) {
    configure_limits(manager, text, "enabled = false\n").await;
}

/// [`configure_fixture`] with disk protection on, for the tests that feed
/// its monitor readings, with admission reading every filesystem as free.
pub async fn configure_disk_fixture(manager: &Manager, text: &str) {
    crate::daemon::capacity::fake_disk_free(u64::MAX);
    configure_limits(manager, text, "enabled = true\n").await;
}

/// Replace the fixture's global config with `text`, a fixed agent limit
/// and `disk`, the body of `[machine.overload.disk]`.
async fn configure_limits(manager: &Manager, text: &str, disk: &str) {
    super::configure(
        manager,
        &format!("{text}\n[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\n{disk}"),
    )
    .await;
}

pub struct FixtureRoot {
    _root: tempfile::TempDir,
    _process_file_guard: std::fs::File,
}

pub async fn fixture() -> (
    FixtureRoot,
    Arc<Manager>,
    crate::model::Workspace,
    tokio::task::JoinHandle<()>,
) {
    // Native ownership scans cover the whole process table. Another fixture's
    // terminating process may have an unreadable environment before it
    // disappears, so fixtures take turns through a file lock, which tests in
    // other processes see too; .config/nextest.toml groups their tests.
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
    // Restores start through the agent queue, which runs no worker here.
    crate::daemon::fake_workers::fake();
    let (root, manager) = manager().await;
    configure_fixture(&manager, "[cleanup]\ngrace_minutes = 0").await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
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
            _process_file_guard: process_file_guard,
        },
        manager,
        workspace,
        serving,
    )
}

/// Serve `manager` on its socket as the daemon does, replacing a socket left
/// by an earlier fixture server.
pub fn serve_fixture(manager: Arc<Manager>) -> tokio::task::JoinHandle<()> {
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

/// Record a chat without an agent in the checkout of the fixture's
/// repository, whose agent restores through the configured command as the
/// fixture's workspace's does, returning it with the checkout.
pub async fn chat_fixture(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> (crate::model::Chat, PathBuf) {
    let repository = workspace.repository().clone();
    let checkout = (manager.repositories.repository(repository.clone()))
        .await
        .unwrap()
        .path;
    install_test_script(
        checkout.join("resume.sh"),
        "#!/bin/sh\nprintf restored > restored\n",
    );
    fs::write(
        checkout.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./resume.sh']\n",
    )
    .unwrap();
    let new = crate::daemon::chats::NewChat {
        name: "talk".into(),
        agent: "fixture".into(),
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
        priority: None,
    };
    let chat = (manager.start_new_chat(Some(repository.into()), None, new))
        .await
        .unwrap();
    (chat, checkout)
}

/// Where a fixture's agent runs: its space, and the directory it starts in,
/// where it writes its markers.
#[derive(Clone, Copy)]
pub struct Site<'a> {
    pub id: &'a SpaceId,
    pub dir: &'a Path,
}

impl<'a> From<&'a crate::model::Workspace> for Site<'a> {
    fn from(workspace: &'a crate::model::Workspace) -> Self {
        Self {
            id: &workspace.id,
            dir: &workspace.path,
        }
    }
}

#[track_caller]
pub fn wait_started<'a>(
    manager: &'a Manager,
    site: impl Into<Site<'a>>,
) -> impl Future<Output = ()> + 'a {
    wait_launched(manager, site.into(), "started", 1)
}

/// Waits until the child has written `marker`, so its TERM trap is set, and
/// the daemon has recorded the process groups of `executions` executions. The
/// marker alone can appear before the wrapper's start report is recorded, and
/// a stop sent then reaches an execution that never paused and whose launch
/// is unproven.
#[track_caller]
pub fn wait_launched<'a>(
    manager: &'a Manager,
    site: impl Into<Site<'a>>,
    marker: &'a str,
    executions: usize,
) -> impl Future<Output = ()> + 'a {
    let site = site.into();
    bounded(async move {
        loop {
            let id = site.id.clone();
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
            if site.dir.join(marker).is_file() && recorded >= executions {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// A pause clears the process group that `wait_started` saw recorded.
#[track_caller]
pub fn wait_paused<'a>(
    manager: &'a Manager,
    site: impl Into<Site<'a>>,
) -> impl Future<Output = ()> + 'a {
    let site = site.into();
    bounded(async move {
        loop {
            let id = site.id.clone();
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

/// Report `session` from the workspace's running agent, as its hooks do.
pub async fn report_session(manager: &Manager, workspace: &crate::model::Workspace, session: &str) {
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

/// Wait until a committed state change satisfies `reached`.
pub async fn store_reaches(manager: &Manager, mut reached: impl AsyncFnMut() -> bool) {
    // Subscribe before the first check so a concurrent commit cannot be missed.
    let mut changed = manager.store.watch_changes();
    bounded(async {
        while !reached().await {
            changed.changed().await.unwrap();
        }
    })
    .await;
}

/// Set the recorded process group of the workspace's only execution.
pub async fn record_group(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    group: Option<u32>,
) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET group_id=?2 WHERE space_id=?1",
                rusqlite::params![id, group],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The agent the daemon recorded for the execution `id`.
pub async fn execution_agent(manager: &Manager, id: ExecutionId) -> Option<crate::agent::Agent> {
    manager
        .store
        .run(move |db| store::execution_agent(db, &id))
        .await
        .unwrap()
}

/// Open the state as a restarted daemon does before it serves.
pub async fn restart(manager: &Manager) -> Arc<Manager> {
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    crate::daemon::quarantine::interrupted_operations(&reopened.store)
        .await
        .unwrap();
    reopened
}

/// The executions the daemon records for `workspace`.
pub async fn executions(manager: &Manager, workspace: &crate::model::Workspace) -> Vec<Execution> {
    manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions
}

/// The recovery record of the execution `id` in `workspace`.
pub fn record(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    id: &ExecutionId,
) -> Option<Record> {
    Record::load(&record_path(&manager.paths, &workspace.id, id)).unwrap()
}

/// The messages of the notifications that a recovery is blocked.
pub async fn blocked(manager: &Manager) -> Vec<String> {
    manager
        .notifications
        .list(false, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|notification| notification.kind == AlertKind::RecoveryBlocked)
        .map(|notification| notification.message)
        .collect()
}
