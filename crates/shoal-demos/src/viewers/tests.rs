use shoal_core::test_support::ProcessGroup;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use tokio::io::AsyncReadExt as _;

use super::*;
use crate::{
    stand_in::Fixture,
    test_support::{ECHO_DEVICE, ECHO_VIEWER, Helper, await_file, helper, launch},
};

/// A workspace running the simulator demo `app` on device `UDID-1` with
/// `viewer`, its port reserved, and the demo as listings show it.
async fn running(fixture: &Fixture, viewer: Option<&[&str]>) -> (SpaceId, Running) {
    let workspace = fixture.workspace("tried").await;
    let demo = fixture.demo(&workspace.id, launch(viewer)).await;
    reserve(fixture, &demo).await;
    (workspace.id, demo)
}

/// Open `demo`'s viewer as the user.
async fn open(fixture: &Fixture, demo: &Running) -> Result<u16> {
    fixture
        .demos
        .open_viewer(&*fixture.daemon, demo, None)
        .await
}

/// The port reserved for `demo`'s viewer.
async fn reserve(fixture: &Fixture, demo: &Running) -> u16 {
    (reserve_viewer_port(&*fixture.daemon, demo, None))
        .await
        .unwrap()
}

async fn listed(fixture: &Fixture, workspace: &SpaceId) -> Running {
    let selector = SpaceSelector::id(workspace);
    let demos = fixture.demos.list(&*fixture.daemon, Some(&selector)).await;
    demos.unwrap().into_iter().next().expect("the demo runs")
}

/// What the viewer on `port` writes to a connection.
async fn read(port: u16) -> String {
    let mut connection = connect(port).await.expect("the viewer takes connections");
    let mut received = String::new();
    connection.read_to_string(&mut received).await.unwrap();
    received
}

/// Record for `demo` a serving viewer an earlier daemon started whose
/// leader exited, leaving a helper of its group without the demo's marker,
/// as one whose environment cannot be read is: nothing proves the helper
/// the demo's. Returns the leader and the helper, which runs until the test
/// ends.
async fn unprovable_viewer(fixture: &Fixture, demo: &Running) -> (Identity, Helper) {
    let port = reserve(fixture, demo).await;
    let root = fixture.root.path();
    let (pid_file, gate) = (root.join("helper.pid"), root.join("exit"));
    let unmarked = format!("env -u {} {}", env::EXECUTION_ID, helper(&pid_file));
    let script = format!("{unmarked}; {}; exit 0", await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = process::capture(earlier.id()).unwrap().unwrap();
    let helper = Helper::read(&pid_file).await;
    let serving = Viewer {
        port,
        process: process.clone(),
        serving: true,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &serving))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let _ = earlier.wait();
    (process, helper)
}

/// The process of the viewer recorded for `demo`.
async fn viewer_process(fixture: &Fixture, demo: &Running) -> Identity {
    let viewer = fixture.demos.running_viewer(&demo.execution_id).await;
    viewer.unwrap().expect("the viewer runs").process
}

/// Run `future` with the clock paused until `resumed` holds, checked every
/// [`START_POLL`] of paused time, and with real time from then on.
async fn paused_until<T>(future: impl Future<Output = T>, resumed: impl Fn() -> bool) -> T {
    tokio::time::pause();
    let mut future = std::pin::pin!(future);
    while !resumed() {
        tokio::select! {
            output = &mut future => {
                tokio::time::resume();
                return output;
            }
            () = tokio::time::sleep(START_POLL) => {}
        }
    }
    tokio::time::resume();
    future.await
}

#[tokio::test]
async fn the_first_open_starts_the_viewer_on_the_demo_s_device_and_later_ones_reuse_it() {
    let fixture = Fixture::new().await;
    let (workspace, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let unopened = demo.viewer.clone().expect("a simulator demo has a viewer");
    assert_eq!((unopened.tool.as_str(), unopened.url), ("python3", None));
    let port = open(&fixture, &demo).await.unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let opened = listed(&fixture, &workspace).await.viewer.unwrap();
    assert_eq!(opened.url, Some(format!("http://localhost:{port}")));
    let process = viewer_process(&fixture, &demo).await;
    assert_eq!(open(&fixture, &demo).await.unwrap(), port);
    assert_eq!(viewer_process(&fixture, &demo).await, process);
}

#[tokio::test]
async fn an_open_waits_for_a_viewer_an_earlier_daemon_left_starting() {
    let fixture = Fixture::new().await;
    let (workspace, demo) = running(&fixture, Some(&["false"])).await;
    let port = reserve(&fixture, &demo).await;
    let gate = fixture.root.path().join("serve");
    let earlier = ProcessGroup::spawn(std::process::Command::new("python3").args([
        "-c",
        ECHO_DEVICE,
        &port.to_string(),
        "UDID-1",
        gate.to_str().unwrap(),
    ]));
    let process = process::capture(earlier.id()).unwrap().unwrap();
    let starting = Viewer {
        port,
        process,
        serving: false,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    let opening = {
        let (demos, daemon, demo) = (fixture.demos.clone(), fixture.daemon.clone(), demo.clone());
        tokio::spawn(async move { demos.open_viewer(&*daemon, &demo, None).await })
    };
    std::fs::write(&gate, "").unwrap();
    assert_eq!(opening.await.unwrap().unwrap(), port);
    let listed = listed(&fixture, &workspace).await.viewer.unwrap();
    assert_eq!(listed.url, Some(format!("http://localhost:{port}")));
    earlier.kill();
}

#[tokio::test]
async fn a_viewer_an_earlier_daemon_started_is_the_demo_s_until_it_exits() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(&["false"])).await;
    let port = reserve(&fixture, &demo).await;
    let earlier = ProcessGroup::spawn(std::process::Command::new("python3").args([
        "-c",
        ECHO_DEVICE,
        &port.to_string(),
        "UDID-1",
    ]));
    let process = process::capture(earlier.id()).unwrap().unwrap();
    let viewer = Viewer {
        port,
        process,
        serving: true,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &viewer))
        .await
        .unwrap();
    // Its command would fail, so the open reuses the recorded viewer.
    assert_eq!(open(&fixture, &demo).await.unwrap(), port);
    earlier.kill();
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
}

#[tokio::test]
async fn a_demo_whose_execution_is_completing_starts_no_viewer() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    fixture.daemon.complete(&demo.execution_id);
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = fixture
        .demos
        .running_viewer(&demo.execution_id)
        .await
        .unwrap();
    assert!(viewer.is_none(), "a viewer started for an ending demo");
}

#[tokio::test]
async fn a_viewer_writes_its_errors_where_no_daemon_needs_to_read_them() {
    let fixture = Fixture::new().await;
    let (workspace, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let port = open(&fixture, &demo).await.unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let log = fixture.paths.viewer_log(&workspace, &demo.launch.name);
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
    let fixture = Fixture::new().await;
    let pid_file = fixture.root.path().join("helper.pid");
    let script = format!("{}; exit 3", helper(&pid_file));
    let viewer = ["sh", "-c", script.as_str()];
    let (_, demo) = running(&fixture, Some(&viewer)).await;
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    Helper::read(&pid_file).await.assert_stopped().await;
}

#[tokio::test]
async fn a_recovered_viewer_that_exits_before_serving_takes_what_it_started_along() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(&["false"])).await;
    let port = reserve(&fixture, &demo).await;
    let (pid_file, gate) = (
        fixture.root.path().join("helper.pid"),
        fixture.root.path().join("exit"),
    );
    // As an earlier daemon left it: marked as the demo's execution and
    // recorded as starting, its leader exiting once recorded.
    let script = format!("{}; {}; exit 0", helper(&pid_file), await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = process::capture(earlier.id()).unwrap().unwrap();
    let helper = Helper::read(&pid_file).await;
    let starting = Viewer {
        port,
        process: process.clone(),
        serving: false,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    while !process::unreaped(&process).unwrap() {
        tokio::time::sleep(START_POLL).await;
    }
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    helper.assert_stopped().await;
    let _ = earlier.wait();
}

#[tokio::test]
async fn a_viewer_an_earlier_daemon_left_starting_that_never_serves_is_replaced() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let port = reserve(&fixture, &demo).await;
    // A process that runs but never takes a connection on the port.
    let stuck = ProcessGroup::spawn(std::process::Command::new("sleep").arg("300"));
    let process = process::capture(stuck.id()).unwrap().unwrap();
    let starting = Viewer {
        port,
        process: process.clone(),
        serving: false,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &starting))
        .await
        .unwrap();
    // Paused only until the wait for it to serve reaches its limit and the
    // stuck viewer is killed, so the stop's own guards keep real time.
    let opening = open(&fixture, &demo);
    let refused = paused_until(opening, || !runs(&process))
        .await
        .err()
        .unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    assert!(!runs(&process), "the stuck viewer still runs");
    // The next open starts a viewer of its own.
    assert_eq!(open(&fixture, &demo).await.unwrap(), port);
    assert_ne!(viewer_process(&fixture, &demo).await, process);
    stuck.kill();
}

#[tokio::test]
async fn an_open_after_a_recorded_viewer_exited_stops_what_it_left() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let port = reserve(&fixture, &demo).await;
    let (pid_file, gate) = (
        fixture.root.path().join("helper.pid"),
        fixture.root.path().join("exit"),
    );
    // A viewer an earlier daemon recorded as serving, whose leader exited
    // and left a helper of its group.
    let script = format!("{}; {}; exit 0", helper(&pid_file), await_file(&gate));
    let mut earlier = ProcessGroup::spawn(
        std::process::Command::new("sh")
            .args(["-c", &script])
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = process::capture(earlier.id()).unwrap().unwrap();
    let helper = Helper::read(&pid_file).await;
    let serving = Viewer {
        port,
        process: process.clone(),
        serving: true,
    };
    (fixture.demos.record_viewer(&demo.execution_id, &serving))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let _ = earlier.wait();
    assert_eq!(open(&fixture, &demo).await.unwrap(), port);
    helper.assert_stopped().await;
    assert_ne!(viewer_process(&fixture, &demo).await, process);
}

#[tokio::test]
async fn an_earlier_viewer_whose_group_cannot_be_proven_stopped_keeps_its_record() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let (process, helper) = unprovable_viewer(&fixture, &demo).await;
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let kept = fixture
        .demos
        .recorded_viewer(&demo.execution_id)
        .await
        .unwrap();
    assert_eq!(kept.map(|viewer| viewer.process), Some(process));
    assert!(
        helper.process.as_ref().is_some_and(runs),
        "the unproven helper was stopped"
    );
}

#[tokio::test]
async fn a_viewer_port_another_process_listens_on_starts_no_viewer() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, Some(ECHO_VIEWER)).await;
    let port = reserve(&fixture, &demo).await;
    let _other = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = fixture
        .demos
        .running_viewer(&demo.execution_id)
        .await
        .unwrap();
    assert!(viewer.is_none(), "a viewer started on a port in use");
}

#[tokio::test]
async fn a_viewer_that_exits_fails_the_open_with_what_it_printed() {
    let fixture = Fixture::new().await;
    let viewer = ["sh", "-c", "echo 'no device {udid}' >&2; exit 3"];
    let (_, demo) = running(&fixture, Some(&viewer)).await;
    let refused = open(&fixture, &demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
    assert!(
        refused.to_string().contains("no device UDID-1"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn a_demo_whose_viewer_is_off_is_refused_one() {
    let fixture = Fixture::new().await;
    let (_, demo) = running(&fixture, None).await;
    assert!(demo.viewer.is_none());
    let refused = open(&fixture, &demo).await.err().unwrap();
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
    let leader = process::capture(child.id().unwrap()).unwrap().unwrap();
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
