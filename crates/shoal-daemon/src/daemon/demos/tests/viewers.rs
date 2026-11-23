//! The viewers of simulator demos across subsystems: the runs and ends of
//! the executions that run their demos, and the ports leases reserve.
use std::time::Duration;

use shoal_core::test_support::ProcessGroup;
use shoal_demos::{
    connect,
    daemon::{Ports, Reserved},
    test_support::{
        ECHO_DEVICE, ECHO_VIEWER, Helper, POLL as START_POLL, await_file, helper, launch,
        record_viewer, runs,
    },
    viewers::Viewer,
};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::{demos::Running, space_names::SpaceSelector};
use tokio::io::AsyncReadExt as _;

use crate::{
    config::repo::ConflictPolicy,
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope::Caller,
        test_support::{manager, repository},
    },
    env,
    model::{PortRequest, Workspace},
    process::identity::{self as process, Identity, IdentityExt as _},
    refusal::Reason,
};

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
) -> (Workspace, Running) {
    let workspace = workspace(manager, root).await;
    let wrapper = process::capture(std::process::id()).unwrap();
    manager
        .agents
        .begin_execution_running(
            manager,
            workspace.id.as_str(),
            wrapper,
            ExecutionKind::Command,
            None,
            Some(launch(viewer)),
        )
        .await
        .unwrap();
    let demo = listed(manager, &workspace.id).await;
    (workspace, demo)
}

async fn workspace(manager: &Manager, root: &std::path::Path) -> Workspace {
    let checkout = repository(root, "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "tried".into(), None, None, None)
        .await
        .unwrap()
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

/// The port reserved for `demo`'s viewer, as an open reserves it.
async fn reserve_viewer_port(manager: &Manager, demo: &Running) -> u16 {
    let request = PortRequest {
        port: None,
        env_var: None,
        reason: None,
        on_conflict: Some(ConflictPolicy::Auto),
    };
    let name = demo.launch.name.clone();
    let reserved = manager.reserve_port(&demo.workspace_id, name, request, None);
    match reserved.await.unwrap() {
        Reserved::Port(port) => port,
        _ => panic!("the viewer's port is not reserved"),
    }
}

async fn listed(manager: &Manager, workspace: &SpaceId) -> Running {
    let selector = SpaceSelector::id(workspace);
    let demos = manager.demos.list(manager, Some(&selector)).await;
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
    let identity = process::capture(child.id()).unwrap().unwrap();
    child.kill();
    manager
        .agents
        .record_execution_child(execution.clone(), Some(identity.clone()), identity.pid)
        .await
        .unwrap();
}

/// Record for `demo` a serving viewer an earlier daemon started whose
/// leader exited, leaving a helper of its group without the demo's marker,
/// as one whose environment cannot be read is: nothing proves the helper
/// the demo's. Returns the leader and the helper, which runs until the test
/// ends.
async fn unprovable_viewer(
    manager: &Manager,
    root: &std::path::Path,
    demo: &Running,
) -> (Identity, Helper) {
    let port = reserve_viewer_port(manager, demo).await;
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
    (record_viewer(&manager.demos, &demo.execution_id, &serving))
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    let _ = earlier.wait();
    (process, helper)
}

/// The process of the viewer recorded for `demo`.
async fn viewer_process(manager: &Manager, demo: &Running) -> Identity {
    let viewer = manager.demos.running_viewer(&demo.execution_id).await;
    viewer.unwrap().expect("the viewer runs").process
}

#[tokio::test]
async fn the_viewer_outlives_the_demo_s_runs_and_ends_with_the_demo() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = manager
        .demos
        .open_viewer(&*manager, &demo, None)
        .await
        .unwrap();
    // Between runs the daemon stops what the exited run left, which the
    // viewer, marked as the demo's execution, is not.
    exited_run(&manager, &demo.execution_id).await;
    manager
        .agents
        .release_exited_child(&*manager, &demo.execution_id, &workspace)
        .await
        .unwrap();
    assert_eq!(read(port).await, "UDID-1");
    // The demo's end stops it with the rest of its processes.
    let process = viewer_process(&manager, &demo).await;
    manager
        .agents
        .stop_exited_processes(&*manager, &demo.execution_id)
        .await;
    let stopped = async {
        while !matches!(
            manager.demos.running_viewer(&demo.execution_id).await,
            Ok(None)
        ) {
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
        tokio::spawn(async move { manager.demos.open_viewer(&*manager, &demo, None).await })
    };
    // The viewer runs, recorded, but does not serve until the gate opens.
    while manager
        .demos
        .running_viewer(&demo.execution_id)
        .await
        .unwrap()
        .is_none()
    {
        tokio::time::sleep(START_POLL).await;
    }
    exited_run(&manager, &demo.execution_id).await;
    manager
        .agents
        .release_exited_child(&*manager, &demo.execution_id, &workspace)
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
    let spawning = manager.demos.viewer_spawn_lock(&demo.execution_id);
    let spawning = spawning.lock().await;
    // A process marked as the demo's, as a viewer is before its record.
    let viewer = ProcessGroup::spawn(
        std::process::Command::new("sleep")
            .arg("30")
            .env(env::EXECUTION_ID, &demo.execution_id),
    );
    let process = process::capture(viewer.id()).unwrap().unwrap();
    let releasing = {
        let (manager, execution) = (manager.clone(), demo.execution_id.clone());
        tokio::spawn(async move {
            manager
                .agents
                .release_exited_child(&*manager, &execution, &workspace)
                .await
        })
    };
    let recorded = Viewer {
        port: 1,
        process: process.clone(),
        serving: false,
    };
    (record_viewer(&manager.demos, &demo.execution_id, &recorded))
        .await
        .unwrap();
    drop(spawning);
    releasing.await.unwrap().unwrap();
    assert!(runs(&process), "the release stopped the recorded viewer");
    viewer.kill();
}

#[tokio::test]
async fn a_scoped_open_waits_for_approval_of_a_viewer_port_that_needs_it() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = started(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let config = "[ports.app]\nrequires_approval = true\n";
    std::fs::write(workspace.path.join(".shoal.toml"), config).unwrap();
    let caller = Caller::of_space(workspace.id.clone());
    let refused = manager
        .demos
        .open_viewer(&*manager, &demo, Some(&caller))
        .await
        .err()
        .unwrap();
    assert_eq!(Reason::of(&refused), Some(Reason::NotReady), "{refused:#}");
    let viewer = manager
        .demos
        .running_viewer(&demo.execution_id)
        .await
        .unwrap();
    assert!(
        viewer.is_none(),
        "the viewer started without its port's approval"
    );
}

#[tokio::test]
async fn forgetting_a_demo_s_execution_stops_its_viewer() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    manager
        .demos
        .open_viewer(&*manager, &demo, None)
        .await
        .unwrap();
    let process = viewer_process(&manager, &demo).await;
    // As a stop of a demo whose wrapper is away ends it.
    manager
        .agents
        .forget_execution(&*manager, &demo.execution_id)
        .await
        .unwrap();
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
async fn a_demo_whose_viewer_cannot_be_proven_stopped_keeps_its_records() {
    let (temp, manager) = manager().await;
    let (workspace, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let _helper = unprovable_viewer(&manager, temp.path(), &demo).await;
    // As a stop of a demo whose wrapper is away ends it.
    assert!(
        manager
            .agents
            .forget_execution(&*manager, &demo.execution_id)
            .await
            .is_err()
    );
    let still = listed(&manager, &workspace).await;
    assert_eq!(still.execution_id, demo.execution_id);
}

#[tokio::test]
async fn a_demo_that_ends_while_its_viewer_cannot_be_proven_stopped_keeps_its_records() {
    use crate::daemon::execution_connection::{ExecutionContext, execute};
    use crate::protocol::{Control, ExecutionEvent, Response};
    use tokio::io::BufReader;
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, temp.path()).await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let context = ExecutionContext {
        space: SpaceSelector::id(&workspace.id),
        wrapper: wrapper.clone(),
        kind: ExecutionKind::Command,
        agent: None,
        recover: false,
        reattach: true,
        caller: None,
        queue_entry: None,
        demo: Some(launch(Some(ECHO_VIEWER))),
        origin: None,
        merge: None,
    };
    let operation = manager.gates.owned_operation().await;
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        context,
        operation,
    ));
    let mut wrapper_side = BufReader::new(client);
    let _: Response = crate::protocol::read_buffered(&mut wrapper_side)
        .await
        .unwrap();
    let started = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: false,
        restored_from: Vec::new(),
        reports_status: false,
    };
    crate::protocol::write(wrapper_side.get_mut(), &started)
        .await
        .unwrap();
    let control: Control = crate::protocol::read_buffered(&mut wrapper_side)
        .await
        .unwrap();
    assert!(matches!(control, Control::Started));
    let demo = listed(&manager, &workspace.id).await;
    let _helper = unprovable_viewer(&manager, temp.path(), &demo).await;
    let finished = ExecutionEvent::Finished {
        exit_code: 0,
        memory: None,
    };
    crate::protocol::write(wrapper_side.get_mut(), &finished)
        .await
        .unwrap();
    let control: Control = crate::protocol::read_buffered(&mut wrapper_side)
        .await
        .unwrap();
    assert!(
        matches!(control, Control::Finished { complete: false }),
        "the demo ended as proven"
    );
    let still = listed(&manager, &workspace.id).await;
    assert_eq!(still.execution_id, demo.execution_id);
    drop(wrapper_side);
    let _ = executing.await;
}

#[tokio::test]
async fn a_viewer_of_an_earlier_boot_leaves_a_group_that_took_its_number_alone() {
    let (temp, manager) = manager().await;
    let (_, demo) = running(&manager, temp.path(), Some(ECHO_VIEWER)).await;
    let port = reserve_viewer_port(&manager, &demo).await;
    // After a restart of the machine, the viewer's group number names an
    // unrelated live group.
    let unrelated = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let live = process::capture(unrelated.id()).unwrap().unwrap();
    let earlier = Viewer {
        port,
        process: live.clone().recorded_in("an-earlier-boot"),
        serving: true,
    };
    (record_viewer(&manager.demos, &demo.execution_id, &earlier))
        .await
        .unwrap();
    manager
        .agents
        .forget_execution(&*manager, &demo.execution_id)
        .await
        .unwrap();
    assert!(runs(&live), "the unrelated group was stopped");
    unrelated.kill();
}
