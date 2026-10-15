use std::{
    io::{Read, Seek},
    path::Path,
    process::{Child, Command, Stdio},
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
        let mut starting = None;
        support::wait::wait_for("daemon startup", || {
            let (daemon, stderr) = starting.get_or_insert_with(|| {
                let stderr = tempfile::tempfile().unwrap();
                let child = launch.stderr(stderr.try_clone().unwrap()).spawn().unwrap();
                (Self { child }, stderr)
            });
            if let Some(status) = daemon.child.try_wait().unwrap() {
                let mut output = String::new();
                stderr.rewind().unwrap();
                stderr.read_to_string(&mut output).unwrap();
                // A killed daemon's lock outlives it while a child it forked has
                // not reached exec yet, so its replacement retries that refusal.
                assert!(
                    output.contains("a daemon already owns this state directory"),
                    "daemon exited during startup ({status}): {output}"
                );
                starting = None;
                return None;
            }
            if support::cli(root)
                .args(["daemon", "status"])
                .output()
                .unwrap()
                .status
                .success()
            {
                return starting.take().map(|(daemon, _)| daemon);
            }
            None
        })
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
