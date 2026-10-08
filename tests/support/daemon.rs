use std::{
    io::{Read, Seek},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::support;

/// Own the child before checking readiness, so startup panics also reap it.
/// Store this guard before its TempDir in fixtures: fields drop in declaration order.
pub struct DaemonGuard {
    pub child: Child,
}

impl DaemonGuard {
    /// Configure per-test daemon environment on `launch` before calling this.
    pub fn start(root: &Path, launch: &mut Command) -> Self {
        launch.args(["daemon", "run"]).stdout(Stdio::null());
        // A hang guard only: startup speed depends on runner load.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let mut stderr = tempfile::tempfile().unwrap();
            let child = launch.stderr(stderr.try_clone().unwrap()).spawn().unwrap();
            let mut daemon = Self { child };
            let Err(status) = daemon.wait_ready(root, deadline) else {
                return daemon;
            };
            let mut output = String::new();
            stderr.rewind().unwrap();
            stderr.read_to_string(&mut output).unwrap();
            // A killed daemon's lock outlives it while a child it forked has
            // not reached exec yet, so its replacement retries that refusal.
            assert!(
                output.contains("a daemon already owns this state directory")
                    && Instant::now() < deadline,
                "daemon exited during startup ({status}): {output}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_ready(&mut self, root: &Path, deadline: Instant) -> Result<(), ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Err(status);
            }
            if support::cli(root)
                .args(["daemon", "status"])
                .output()
                .unwrap()
                .status
                .success()
            {
                return Ok(());
            }
            assert!(Instant::now() < deadline, "daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn restart(&mut self, root: &Path, launch: &mut Command) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
        *self = Self::start(root, launch);
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
