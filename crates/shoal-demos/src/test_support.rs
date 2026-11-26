//! Fixtures the tests of demos share with the daemon's tests of demos.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::demos::{Launch, Preset, SIM_UDID};

use crate::{
    Demos,
    process::identity::{self as process, Identity},
    viewers::{START_POLL, Viewer},
};

/// A viewer that writes the device it was given to each connection on the
/// port it was given, once the file it may name exists. It exits with the
/// test that started it.
pub const ECHO_DEVICE: &str = r#"
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

pub const ECHO_VIEWER: &[&str] = &["python3", "-c", ECHO_DEVICE, "{port}", "{udid}"];

/// How often a test checks what a viewer does.
pub const POLL: Duration = START_POLL;

/// The simulator demo `app` on device `UDID-1` with `viewer`.
pub fn launch(viewer: Option<&[&str]>) -> Launch {
    Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(Preset::Simulator),
        open: None,
        scheme: None,
        viewer_command: viewer.map(|viewer| viewer.iter().map(|arg| (*arg).to_owned()).collect()),
        environment: BTreeMap::from([(SIM_UDID.to_owned(), "UDID-1".to_owned())]),
        origin: shoal_core::sessions::Origin::created(
            shoal_core::sessions::HostName::Zmx,
            "app".into(),
        ),
    }
}

/// Whether `process` runs, as a viewer's leader is taken to.
pub fn runs(process: &Identity) -> bool {
    crate::viewers::runs(process)
}

/// Record `viewer` for the demo `execution` runs, as an earlier daemon
/// left it.
pub async fn record_viewer(demos: &Demos, execution: &ExecutionId, viewer: &Viewer) -> Result<()> {
    demos.record_viewer(execution, viewer).await
}

/// A shell command that starts a helper writing its PID to `pid_file`,
/// which runs well past every wait here unless something stops it. It is
/// the running test binary, whose marker a scan reads where a system
/// binary's environment is hidden, as on macOS; the binary defines it with
/// `shoal_core::helper_tests!`.
pub fn helper(pid_file: &Path) -> String {
    let binary = std::env::current_exe().unwrap();
    format!(
        "'{}' --exact helper_tests::run_until_stopped --ignored >/dev/null 2>&1 & echo $! > {}",
        binary.display(),
        pid_file.display()
    )
}

/// A shell command that waits until `gate` exists.
pub fn await_file(gate: &Path) -> String {
    format!("while [ ! -e {} ]; do sleep 0.05; done", gate.display())
}

/// A helper a test viewer started, killed as the test ends unless the
/// cleanup under test stopped it.
pub struct Helper {
    pid: u32,
    pub process: Option<Identity>,
}

impl Helper {
    /// The helper whose PID `pid_file` holds, once it holds one.
    pub async fn read(pid_file: &Path) -> Self {
        let pid = loop {
            let written = std::fs::read_to_string(pid_file).unwrap_or_default();
            if let Ok(pid) = written.trim().parse::<u32>() {
                break pid;
            }
            tokio::time::sleep(POLL).await;
        };
        let process = process::capture(pid).unwrap();
        Self { pid, process }
    }

    /// Wait, with a hang guard, until the helper no longer runs.
    pub async fn assert_stopped(&self) {
        let Some(process) = &self.process else {
            return;
        };
        let stopped = async {
            while runs(process) {
                tokio::time::sleep(POLL).await;
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
            let _ = process::signal(process, libc::SIGKILL);
        }
    }
}
