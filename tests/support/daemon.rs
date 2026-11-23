use std::{
    fs::File,
    io::{Read, Seek},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
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
        launch
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .process_group(0);
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
                .args(["status"])
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
        self.kill();
        *self = Self::start(root, launch);
    }

    /// Kill the daemon with the commands it runs in its process group, which
    /// launchd would otherwise adopt; one with a hidden environment fails the
    /// setup checks of other tests' daemons. A daemon a test launched itself
    /// may lead no group.
    pub fn kill(&mut self) {
        // While the daemon is unreaped, no other process can take its group ID.
        if matches!(self.child.try_wait(), Ok(None)) {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        self.kill();
    }
}

/// The process group of the CLI processes a fixture spawns, so dropping the
/// fixture stops what a failed test left running, such as a `shoal run`
/// wrapper. It is not the daemon's, which a test kills to simulate a crash
/// that wrappers outlive. A `cat` reading a pipe from the test leads it, so
/// the group lasts across daemon restarts and its leader exits with the test.
pub struct CommandGroup {
    leader: Child,
}

impl CommandGroup {
    pub fn new() -> Self {
        let leader = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        Self { leader }
    }

    /// Run `command` in the group. One that starts its own session leaves it.
    pub fn join(&self, command: &mut Command) {
        let group = self.leader.id() as libc::pid_t;
        // SAFETY: setpgid is async-signal-safe and touches no memory.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, group) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// SIGTERM first, and SIGKILL only once the group is empty or the execution
/// stop grace has passed: a wrapper killed at once would leave its command's
/// own process group running.
impl Drop for CommandGroup {
    fn drop(&mut self) {
        // While the leader is unreaped, no other process can take its group ID.
        let group = -(self.leader.id() as i32);
        unsafe { libc::kill(group, libc::SIGTERM) };
        let deadline = Instant::now() + 2 * EXECUTION_STOP_GRACE;
        while Instant::now() < deadline && group_runs(self.leader.id()) {
            std::thread::sleep(Duration::from_millis(25));
        }
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = self.leader.kill();
        let _ = self.leader.wait();
    }
}

/// The daemon's `timing::EXECUTION_STOP_GRACE`, which a wrapper gives its
/// command after SIGTERM before it kills it.
const EXECUTION_STOP_GRACE: Duration = Duration::from_secs(2);

/// Whether a process of process group `group` runs, other than zombies.
fn group_runs(group: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-ax", "-o", "pgid=,stat="])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().and_then(|pgid| pgid.parse().ok()) == Some(group)
            && fields.next().is_some_and(|stat| !stat.starts_with('Z'))
    })
}
