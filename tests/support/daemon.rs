use std::{
    path::Path,
    process::{Child, Command, Stdio},
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
        let child = launch
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self { child };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            if support::cli(root)
                .args(["daemon", "status"])
                .output()
                .unwrap()
                .status
                .success()
            {
                return daemon;
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
