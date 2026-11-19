use std::collections::BTreeMap;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::demos::{Launch, Preset, SIM_UDID};
use tokio::io::AsyncReadExt as _;

use super::*;
use crate::daemon::{
    executions::ExecutionKind,
    test_support::{manager, repository},
};

/// A viewer that writes the device it was given to each connection on the
/// port it was given.
const ECHO_DEVICE: &str = r#"
import socket, sys
server = socket.create_server(("127.0.0.1", int(sys.argv[1])))
while True:
    connection, _ = server.accept()
    connection.sendall(sys.argv[2].encode())
    connection.close()
"#;

/// A workspace running the simulator demo `app` on device `UDID-1` with
/// `viewer`, and the demo as listings show it.
async fn running(
    manager: &Manager,
    root: &std::path::Path,
    viewer: Option<&[&str]>,
) -> (SpaceId, Running) {
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
    manager
        .begin_execution_running(
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(launch),
        )
        .await
        .unwrap();
    let demo = listed(manager, &workspace.id).await;
    (workspace.id, demo)
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

#[tokio::test]
async fn the_first_open_starts_the_viewer_on_the_demo_s_device_and_later_ones_reuse_it() {
    let (temp, manager) = manager().await;
    let viewer = ["python3", "-c", ECHO_DEVICE, "{port}", "{udid}"];
    let (workspace, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let unopened = demo.viewer.clone().expect("a simulator demo has a viewer");
    assert_eq!((unopened.tool.as_str(), unopened.url), ("python3", None));
    let port = manager.open_viewer(&demo).await.unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let opened = listed(&manager, &workspace).await.viewer.unwrap();
    assert_eq!(opened.url, Some(format!("http://localhost:{port}")));
    let group = manager.viewers.group(&demo.execution_id);
    assert_eq!(manager.open_viewer(&demo).await.unwrap(), port);
    assert_eq!(manager.viewers.group(&demo.execution_id), group);
}

#[tokio::test]
async fn the_viewer_outlives_the_demo_s_runs_and_ends_with_the_demo() {
    let (temp, manager) = manager().await;
    let viewer = ["python3", "-c", ECHO_DEVICE, "{port}", "{udid}"];
    let (workspace, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let port = manager.open_viewer(&demo).await.unwrap();
    // Between runs the daemon stops what the exited run left, which the
    // viewer, marked as the demo's execution, is not.
    manager
        .release_exited_child(&demo.execution_id, &workspace)
        .await
        .unwrap();
    assert_eq!(read(port).await, "UDID-1");
    let group = manager.viewers.group(&demo.execution_id).unwrap();
    manager.viewers.end(&demo.execution_id);
    crate::process::identity::group_exited(group).await.unwrap();
    assert!(manager.viewers.port(&demo.execution_id).is_none());
}

#[tokio::test]
async fn a_viewer_that_exits_fails_the_open_with_what_it_printed() {
    let (temp, manager) = manager().await;
    let viewer = ["sh", "-c", "echo 'no device {udid}' >&2; exit 3"];
    let (_, demo) = running(&manager, temp.path(), Some(&viewer)).await;
    let refused = manager.open_viewer(&demo).await.err().unwrap();
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
    let refused = manager.open_viewer(&demo).await.err().unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady));
}

#[test]
fn a_missing_serve_sim_names_its_install_command() {
    let missing = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let serve_sim = not_started("serve-sim", missing()).to_string();
    assert!(serve_sim.contains(SERVE_SIM.1), "{serve_sim}");
    let other = not_started("viewer", missing()).to_string();
    assert!(!other.contains(SERVE_SIM.1), "{other}");
}
