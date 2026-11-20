use std::collections::BTreeMap;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::demos::{Launch, Preset, SIM_UDID};
use tokio::io::AsyncReadExt as _;

use super::*;
use crate::daemon::{
    executions::ExecutionKind,
    test_support::{ProcessGroup, manager, repository},
};

/// A viewer that writes the device it was given to each connection on the
/// port it was given, once the file it may name exists. It exits with the
/// test that started it.
const ECHO_DEVICE: &str = r#"
import os, socket, sys, threading, time
parent = os.getppid()
def follow_parent():
    while os.getppid() == parent:
        time.sleep(0.2)
    os._exit(0)
threading.Thread(target=follow_parent, daemon=True).start()
while len(sys.argv) > 3 and not os.path.exists(sys.argv[3]):
    time.sleep(0.05)
server = socket.create_server(("127.0.0.1", int(sys.argv[1])))
while True:
    connection, _ = server.accept()
    connection.sendall(sys.argv[2].encode())
    connection.close()
    print("served", file=sys.stderr, flush=True)
"#;

const ECHO_VIEWER: &[&str] = &["python3", "-c", ECHO_DEVICE, "{port}", "{udid}"];

/// A workspace running the simulator demo `app` on device `UDID-1` with
/// `viewer`, from this process as its wrapper, and the demo as listings
/// show it. The demo's port is one the system gave out, so tests running
/// at once do not reach each other's viewers.
async fn running(
    manager: &Manager,
    root: &std::path::Path,
    viewer: Option<&[&str]>,
) -> (SpaceId, Running) {
    let (workspace, _) = started(manager, root, viewer).await;
    reserve_free_port(manager, &workspace.id).await;
    let demo = listed(manager, &workspace.id).await;
    (workspace.id, demo)
}

/// A workspace running the simulator demo `app` with `viewer`, whose port
/// is not reserved yet, and the demo.
async fn started(
    manager: &Manager,
    root: &std::path::Path,
    viewer: Option<&[&str]>,
) -> (crate::model::Workspace, Running) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "tried".into(), None, None, None)
        .await
        .unwrap();
    let launch = Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(Preset::Simulator),
        open: None,
        scheme: None,
        viewer_command: viewer.map(|viewer| viewer.iter().map(|arg| (*arg).to_owned()).collect()),
        environment: BTreeMap::from([(SIM_UDID.to_owned(), "UDID-1".to_owned())]),
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "app".into()),
    };
    let wrapper = crate::process::identity::capture(std::process::id()).unwrap();
    manager
        .begin_execution_running(
            workspace.id.as_str(),
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch),
        )
        .await
        .unwrap();
    let demo = listed(manager, &workspace.id).await;
    (workspace, demo)
}

/// Reserve the demo's port at one nothing listens on.
async fn reserve_free_port(manager: &Manager, workspace: &SpaceId) {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let request = PortRequest {
        port: Some(port),
        env_var: None,
        reason: None,
        on_conflict: None,
    };
    let workspace = SpaceSelector::id(workspace);
    (manager.leases)
        .acquire_port(manager, &workspace, "app".into(), request, None)
        .await
        .unwrap();
}

async fn listed(manager: &Manager, workspace: &SpaceId) -> Running {
    let demos = manager.demos(Some(&SpaceSelector::id(workspace))).await;
    demos.unwrap().into_iter().next().expect("the demo runs")
}

/// What the viewer on `port` writes to a connection.
async fn read(port: u16) -> String {
    let mut connection = connect(port).await.expect("the viewer takes connections");
    let mut received = String::new();
    connection.read_to_string(&mut received).await.unwrap();
    received
}

/// Record a run of the demo `execution` runs that has exited.
async fn exited_run(manager: &Manager, execution: &ExecutionId) {
    let child = ProcessGroup::spawn(std::process::Command::new("sleep").arg("30"));
    let identity = crate::process::identity::capture(child.id())
        .unwrap()
        .unwrap();
    child.kill();
    manager
        .record_execution_child(execution.clone(), Some(identity.clone()), identity.pid)
        .await
        .unwrap();
}

/// A shell command that starts a helper writing its PID to `pid_file`,
/// which runs well past every wait here unless something stops it. It is
/// this test binary, whose marker a scan reads where a system binary's
/// environment is hidden, as on macOS.
fn helper(pid_file: &std::path::Path) -> String {
    let binary = std::env::current_exe().unwrap();
    format!(
        "'{}' --exact helper_tests::run_until_stopped --ignored >/dev/null 2>&1 & echo $! > {}",
        binary.display(),
        pid_file.display()
    )
}

/// A shell command that waits until `gate` exists.
fn await_file(gate: &std::path::Path) -> String {
    format!("while [ ! -e {} ]; do sleep 0.05; done", gate.display())
}

/// A helper a test viewer started, killed as the test ends unless the
/// cleanup under test stopped it.
struct Helper {
    pid: u32,
    process: Option<Identity>,
}

impl Helper {
    /// The helper whose PID `pid_file` holds, once it holds one.
    async fn read(pid_file: &std::path::Path) -> Self {
        let pid = loop {
            let written = std::fs::read_to_string(pid_file).unwrap_or_default();
            if let Ok(pid) = written.trim().parse::<u32>() {
                break pid;
            }
            tokio::time::sleep(START_POLL).await;
        };
        let process = crate::process::identity::capture(pid).unwrap();
        Self { pid, process }
    }

    /// Wait, with a hang guard, until the helper no longer runs.
    async fn assert_stopped(&self) {
        let Some(process) = &self.process else {
            return;
        };
        let stopped = async {
            while runs(process) {
                tokio::time::sleep(START_POLL).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(60), stopped)
            .await
            .unwrap_or_else(|_| panic!("helper {} still runs", self.pid));
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(process) = &self.process {
            let _ = crate::process::identity::signal(process, libc::SIGKILL);
        }
    }
}

/// The process of the viewer recorded for `demo`.
async fn viewer_process(manager: &Manager, demo: &Running) -> Identity {
    let viewer = manager.running_viewer(&demo.execution_id).await.unwrap();
    viewer.expect("the viewer runs").process
}

#[tokio::test]
async fn the_first_open_starts_the_viewer_on_the_demo_s_device_and_later_ones_reuse_it() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let unopened = demo.viewer.clone().expect("a simulator demo has a viewer");
    assert_eq!((unopened.tool.as_str(), unopened.url), ("python3", None));
    let port = manager.open_viewer(&demo, None).await.unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let opened = listed(&manager, &workspace).await.viewer.unwrap();
    assert_eq!(opened.url, Some(format!("http://localhost:{port}")));
    let process = viewer_process(&manager, &demo).await;
    assert_eq!(manager.open_viewer(&demo, None).await.unwrap(), port);
    assert_eq!(viewer_process(&manager, &demo).await, process);
}

#[tokio::test]
async fn the_viewer_outlives_the_demo_s_runs_and_ends_with_the_demo() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.open_viewer(&demo, None).await.unwrap();
    // Between runs the daemon stops what the exited run left, which the
    // viewer, marked as the demo's execution, is not.
    exited_run(&manager, &demo.execution_id).await;
    manager
        .release_exited_child(&demo.execution_id, &workspace)
        .await
        .unwrap();
    assert_eq!(read(port).await, "UDID-1");
    // The demo's end stops it with the rest of its processes.
    let process = viewer_process(&manager, &demo).await;
    manager.stop_exited_processes(&demo.execution_id).await;
    let stopped = async {
        while !matches!(manager.running_viewer(&demo.execution_id).await, Ok(None)) {
            tokio::time::sleep(START_POLL).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), stopped)
        .await
        .unwrap_or_else(|_| panic!("viewer {} still runs", process.pid));
}

#[tokio::test]
async fn a_run_that_ends_while_the_viewer_starts_spares_it() {
    let (temp, manager) = manager().await;
    let gate = temp.path().join("serve");
    let gate_arg = gate.to_str().unwrap();
    let viewer = ["python3", "-c", ECHO_DEVICE, "{port}", "{udid}", gate_arg];
    let (workspace, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let opening = {
        let (manager, demo) = (manager.clone(), demo.clone());
        tokio::spawn(async move { manager.open_viewer(&demo, None).await })
    };
    // The viewer runs, recorded, but does not serve until the gate opens.
    while manager
        .running_viewer(&demo.execution_id)
        .await
        .unwrap()
        .is_none()
    {
        tokio::time::sleep(START_POLL).await;
    }
    exited_run(&manager, &demo.execution_id).await;
    manager
        .release_exited_child(&demo.execution_id, &workspace)
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let port = opening.await.unwrap().unwrap();
    assert_eq!(read(port).await, "UDID-1");
}

#[tokio::test]
async fn a_run_that_ends_while_the_viewer_spawns_waits_until_it_is_recorded() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    exited_run(&manager, &demo.execution_id).await;
    let spawning = manager.viewers.spawn_lock(&demo.execution_id);
    let spawning = spawning.lock().await;
    // A process marked as the demo's, as a viewer is before its record.
    let viewer = ProcessGroup::spawn(
        std::process::Command::new("sleep")
            .arg("30")
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = crate::process::identity::capture(viewer.id())
        .unwrap()
        .unwrap();
    let releasing = {
        let (manager, execution) = (manager.clone(), demo.execution_id.clone());
        tokio::spawn(async move { manager.release_exited_child(&execution, &workspace).await })
    };
    let recorded = Viewer {
        port: 1,
        process: process.clone(),
        serving: false,
    };
    (manager.record_viewer(&demo.execution_id, &recorded))
        .await
        .unwrap();
    drop(spawning);
    releasing.await.unwrap().unwrap();
    assert!(runs(&process), "the release stopped the recorded viewer");
    viewer.kill();
}

#[tokio::test]
async fn an_open_waits_for_a_viewer_an_earlier_daemon_left_starting() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(&["false"])).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let gate = temp.path().join("serve");
    let earlier = ProcessGroup::spawn(std::process::Command::new("python3").args([
        "-c",
        ECHO_DEVICE,
        &port.to_string(),
        "UDID-1",
        gate.to_str().unwrap(),
    ]));
    let process = crate::process::identity::capture(earlier.id())
        .unwrap()
        .unwrap();
    let starting = Viewer {
        port,
        process,
        serving: false,
    };
    (manager.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    let opening = {
        let (manager, demo) = (manager.clone(), demo.clone());
        tokio::spawn(async move { manager.open_viewer(&demo, None).await })
    };
    std::fs::write(&gate, "").unwrap();
    assert_eq!(opening.await.unwrap().unwrap(), port);
    let listed = listed(&manager, &workspace).await.viewer.unwrap();
    assert_eq!(listed.url, Some(format!("http://localhost:{port}")));
    earlier.kill();
}

#[tokio::test]
async fn a_viewer_an_earlier_daemon_started_is_the_demo_s_until_it_exits() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(&["false"])).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let earlier = ProcessGroup::spawn(std::process::Command::new("python3").args([
        "-c",
        ECHO_DEVICE,
        &port.to_string(),
        "UDID-1",
    ]));
    let process = crate::process::identity::capture(earlier.id())
        .unwrap()
        .unwrap();
    let viewer = Viewer {
        port,
        process,
        serving: true,
    };
    (manager.record_viewer(&demo.execution_id, &viewer))
        .await
        .unwrap();
    // Its command would fail, so the open reuses the recorded viewer.
    assert_eq!(manager.open_viewer(&demo, None).await.unwrap(), port);
    earlier.kill();
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
}

#[tokio::test]
async fn a_scoped_open_waits_for_approval_of_a_viewer_port_that_needs_it() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = started(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let config = "[ports.app]\nrequires_approval = true\n";
    std::fs::write(workspace.path.join(".shoal.toml"), config).unwrap();
    let caller = Caller::of_space(workspace.id.clone());
    let refused = manager
        .open_viewer(&demo, Some(&caller))
        .await
        .err()
        .unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = manager.running_viewer(&demo.execution_id).await.unwrap();
    assert!(
        viewer.is_none(),
        "the viewer started without its port's approval"
    );
}

#[tokio::test]
async fn a_demo_whose_execution_is_completing_starts_no_viewer() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let stopping = crate::daemon::executions::Completing::StoppingSurvivors;
    manager
        .executions
        .set_completing(&demo.execution_id, stopping);
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = manager.running_viewer(&demo.execution_id).await.unwrap();
    assert!(viewer.is_none(), "a viewer started for an ending demo");
}

#[tokio::test]
async fn forgetting_a_demo_s_execution_stops_its_viewer() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    manager.open_viewer(&demo, None).await.unwrap();
    let process = viewer_process(&manager, &demo).await;
    // As a stop of a demo whose wrapper is away ends it.
    manager.forget_execution(&demo.execution_id).await.unwrap();
    let stopped = async {
        while runs(&process) {
            tokio::time::sleep(START_POLL).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), stopped)
        .await
        .unwrap_or_else(|_| panic!("viewer {} still runs", process.pid));
}

#[tokio::test]
async fn a_viewer_writes_its_errors_where_no_daemon_needs_to_read_them() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.open_viewer(&demo, None).await.unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let log = manager.paths.viewer_log(&workspace, &demo.launch.name);
    let written = async {
        while !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("served")
        {
            tokio::time::sleep(START_POLL).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), written)
        .await
        .unwrap_or_else(|_| panic!("{} has no line the viewer wrote", log.display()));
}

#[tokio::test]
async fn a_viewer_that_exits_before_serving_takes_what_it_started_along() {
    let (temp, manager) = manager().await;
    let pid_file = temp.path().join("helper.pid");
    let script = format!("{}; exit 3", helper(&pid_file));
    let viewer = ["sh", "-c", script.as_str()];
    let (_, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    Helper::read(&pid_file).await.assert_stopped().await;
}

#[tokio::test]
async fn a_recovered_viewer_that_exits_before_serving_takes_what_it_started_along() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(&["false"])).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let (pid_file, gate) = (temp.path().join("helper.pid"), temp.path().join("exit"));
    // As an earlier daemon left it: marked as the demo's execution and
    // recorded as starting, its leader exiting once recorded.
    let script = format!("{}; {}; exit 0", helper(&pid_file), await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = crate::process::identity::capture(earlier.id())
        .unwrap()
        .unwrap();
    let helper = Helper::read(&pid_file).await;
    let starting = Viewer {
        port,
        process: process.clone(),
        serving: false,
    };
    (manager.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    while !process::unreaped(&process).unwrap() {
        tokio::time::sleep(START_POLL).await;
    }
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    helper.assert_stopped().await;
    let _ = earlier.wait();
}

#[tokio::test]
async fn a_viewer_an_earlier_daemon_left_starting_that_never_serves_is_replaced() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    // A process that runs but never takes a connection on the port.
    let stuck = ProcessGroup::spawn(std::process::Command::new("sleep").arg("300"));
    let process = crate::process::identity::capture(stuck.id())
        .unwrap()
        .unwrap();
    let starting = Viewer {
        port,
        process: process.clone(),
        serving: false,
    };
    (manager.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    // Paused only for the wait that must reach its limit, so the daemon's
    // own guards elsewhere keep real time.
    tokio::time::pause();
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    tokio::time::resume();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    assert!(!runs(&process), "the stuck viewer still runs");
    // The next open starts a viewer of its own.
    assert_eq!(manager.open_viewer(&demo, None).await.unwrap(), port);
    assert_ne!(viewer_process(&manager, &demo).await, process);
    stuck.kill();
}

#[tokio::test]
async fn an_open_after_a_recorded_viewer_exited_stops_what_it_left() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let (pid_file, gate) = (temp.path().join("helper.pid"), temp.path().join("exit"));
    // A viewer an earlier daemon recorded as serving, whose leader exited
    // and left a helper of its group.
    let script = format!("{}; {}; exit 0", helper(&pid_file), await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = crate::process::identity::capture(earlier.id())
        .unwrap()
        .unwrap();
    let helper = Helper::read(&pid_file).await;
    let serving = Viewer {
        port,
        process: process.clone(),
        serving: true,
    };
    (manager.record_viewer(&demo.execution_id, &serving))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let _ = earlier.wait();
    assert_eq!(manager.open_viewer(&demo, None).await.unwrap(), port);
    helper.assert_stopped().await;
    assert_ne!(viewer_process(&manager, &demo).await, process);
}

#[tokio::test]
async fn an_earlier_viewer_whose_group_cannot_be_proven_stopped_keeps_its_record() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let (pid_file, gate) = (temp.path().join("helper.pid"), temp.path().join("exit"));
    // Its helper lacks the demo's marker, as one whose environment cannot
    // be read does, so nothing proves it the demo's.
    let unmarked = format!("env -u {} {}", env::EXECUTION_ID, helper(&pid_file));
    let script = format!("{unmarked}; {}; exit 0", await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = crate::process::identity::capture(earlier.id())
        .unwrap()
        .unwrap();
    let helper = Helper::read(&pid_file).await;
    let serving = Viewer {
        port,
        process: process.clone(),
        serving: true,
    };
    (manager.record_viewer(&demo.execution_id, &serving))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let _ = earlier.wait();
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let kept = manager.recorded_viewer(&demo.execution_id).await.unwrap();
    assert_eq!(kept.map(|viewer| viewer.process), Some(process));
    assert!(
        helper.process.as_ref().is_some_and(runs),
        "the unproven helper was stopped"
    );
}

#[tokio::test]
async fn a_viewer_port_another_process_listens_on_starts_no_viewer() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager.reserve_viewer_port(&demo, None).await.unwrap();
    let _other = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = manager.running_viewer(&demo.execution_id).await.unwrap();
    assert!(viewer.is_none(), "a viewer started on a port in use");
}

#[tokio::test]
async fn a_viewer_that_exits_fails_the_open_with_what_it_printed() {
    let (temp, manager) = manager().await;
    let viewer = ["sh", "-c", "echo 'no device {udid}' >&2; exit 3"];
    let (_, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
    assert!(
        refused.to_string().contains("no device UDID-1"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn a_demo_whose_viewer_is_off_is_refused_one() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), None).await;
    assert!(demo.viewer.is_none());
    let refused = manager.open_viewer(&demo, None).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
}

#[tokio::test]
async fn stopping_a_viewer_that_exited_unreaped_takes_what_it_started_along() {
    let temp = tempfile::tempdir().unwrap();
    let (pid_file, gate) = (temp.path().join("helper.pid"), temp.path().join("exit"));
    let script = format!("{}; {}; exit 0", helper(&pid_file), await_file(&gate));
    let mut child = Command::new("sh")
        .args(["-c", &script])
        .process_group(0)
        .spawn()
        .unwrap();
    let leader = crate::process::identity::capture(child.id().unwrap())
        .unwrap()
        .unwrap();
    // The leader exits once its identity is known, leaving its helper,
    // and stays unreaped.
    let helper = Helper::read(&pid_file).await;
    std::fs::write(&gate, "").unwrap();
    while !process::unreaped(&leader).unwrap() {
        tokio::time::sleep(START_POLL).await;
    }
    stop_unreaped(&mut child).await;
    helper.assert_stopped().await;
}

#[test]
fn a_failure_shows_a_bounded_tail_of_what_the_viewer_printed() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("viewer.log");
    let huge = "x".repeat(1 << 20);
    std::fs::write(&log, format!("first\n{huge}\nlast\n")).unwrap();
    let shown = printed(&log);
    assert!(
        shown.len() <= ERROR_BYTES as usize + 2,
        "{} bytes",
        shown.len()
    );
    assert!(shown.ends_with("last"), "{}", &shown[shown.len() - 10..]);
    assert!(!shown.contains("first"));
}

#[test]
fn a_missing_serve_sim_names_its_install_command() {
    let missing = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let serve_sim = not_started("serve-sim", missing()).to_string();
    assert!(serve_sim.contains(SERVE_SIM.1), "{serve_sim}");
    let other = not_started("viewer", missing()).to_string();
    assert!(!other.contains(SERVE_SIM.1), "{other}");
}
