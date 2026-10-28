use std::{
    fs::{self, File},
    io::{Read, Seek},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
};

use crate::support;

/// Own the child before checking readiness, so startup panics also reap it.
/// Store this guard before its TempDir in fixtures: fields drop in declaration order.
pub struct DaemonGuard {
    pub child: Child,
    /// The daemon's stderr, shown when the test fails.
    pub stderr: Option<File>,
}

impl DaemonGuard {
    /// Configure per-test daemon environment on `launch` before calling this.
    pub fn start(root: &Path, launch: &mut Command) -> Self {
        launch.args(["daemon", "run"]).stdout(Stdio::null());
        let mut starting = None;
        support::wait::wait_for("daemon startup", || {
            let daemon = starting.get_or_insert_with(|| {
                let stderr = tempfile::tempfile().unwrap();
                let child = launch.stderr(stderr.try_clone().unwrap()).spawn().unwrap();
                Self {
                    child,
                    stderr: Some(stderr),
                }
            });
            if let Some(status) = daemon.child.try_wait().unwrap() {
                // A killed daemon's lock outlives it while a child it forked has
                // not reached exec yet, so its replacement retries that refusal.
                // Any other exit fails here, and the guard reports why.
                assert!(
                    daemon
                        .output()
                        .contains("a daemon already owns this state directory"),
                    "daemon exited during startup ({status})"
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
                return starting.take();
            }
            None
        })
    }

    pub fn restart(&mut self, root: &Path, launch: &mut Command) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
        *self = Self::start(root, launch);
    }

    /// What the daemon wrote to stderr so far.
    fn output(&mut self) -> String {
        let mut output = String::new();
        if let Some(stderr) = &mut self.stderr {
            let _ = stderr.rewind();
            let _ = stderr.read_to_string(&mut output);
        }
        output
    }

    /// Whether the daemon is still running and what it logged, so a failed
    /// request names the daemon's own failure, such as a panic or a crash.
    pub fn report(&mut self) -> String {
        let state = match self.child.try_wait() {
            Ok(Some(status)) => format!("daemon exited ({status})"),
            Ok(None) => "daemon running".to_owned(),
            Err(error) => format!("daemon state unknown: {error}"),
        };
        format!("{state}; its stderr:\n{}", self.output().trim_end())
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.report());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Give the daemon a `zmx`, last on its PATH, when the runner has none, so
/// its startup check for required tools records no alert every test would
/// count. A runner with zmx keeps its own, and the CLI's PATH is unchanged.
#[allow(dead_code)]
pub fn provide_zmx(root: &Path, launch: &mut Command) {
    let path = launch
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value.map(ToOwned::to_owned))
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join("zmx").is_file()) {
        return;
    }
    let dir = root.join("daemon-bin");
    fs::create_dir_all(&dir).unwrap();
    let zmx = dir.join("zmx");
    fs::write(
        &zmx,
        "#!/bin/sh\nif [ \"$1\" = list ]; then exit 0; fi\necho 'zmx is not installed on this test runner' >&2\nexit 127\n",
    )
    .unwrap();
    fs::set_permissions(&zmx, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::env::split_paths(&path).chain([dir])).unwrap();
    launch.env("PATH", path);
}
