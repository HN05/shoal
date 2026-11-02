use std::{
    fs::{self, File},
    io::{Read, Seek},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
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
        self.kill();
        *self = Self::start(root, launch);
    }

    /// Kill the daemon with the commands it runs in its process group, which
    /// launchd would otherwise adopt; one with a hidden environment, such as
    /// `lsof`, fails the setup checks of other tests' daemons. A daemon a test
    /// launched itself may lead no group.
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
#[allow(dead_code)] // Integration test binaries use different subsets of support.
pub struct CommandGroup {
    leader: Child,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
const EXECUTION_STOP_GRACE: Duration = Duration::from_secs(2);

/// Whether a process of process group `group` runs, other than zombies.
#[allow(dead_code)]
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

/// The processes that name a fixture's state directory, such as place
/// watchers, which start their own session and so leave its command group.
/// Dropped after the commands and the daemon stop, it kills them with their
/// groups before the fixture's directory goes: a watcher still polling its
/// host would write into the directory while it is removed and keep it.
#[allow(dead_code)]
pub struct Detached {
    state: PathBuf,
}

#[allow(dead_code)]
impl Detached {
    pub fn new(state: PathBuf) -> Self {
        Self { state }
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        let argument = format!("--state-dir {} ", self.state.display());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = processes_naming(&argument);
            if found.is_empty() || Instant::now() >= deadline {
                break;
            }
            for (pid, group) in found {
                let target = if pid == group { -pid } else { pid };
                unsafe { libc::kill(target, libc::SIGKILL) };
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// The process and group IDs of the processes whose command line contains
/// `text`, other than zombies.
#[allow(dead_code)]
fn processes_naming(text: &str) -> Vec<(i32, i32)> {
    let Ok(output) = Command::new("ps")
        .args(["-ax", "-ww", "-o", "pid=,pgid=,stat=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(text))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let group = fields.next()?.parse().ok()?;
            let stat = fields.next()?;
            (!stat.starts_with('Z')).then_some((pid, group))
        })
        .collect()
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
    write_zmx_stub(&dir);
    let path = std::env::join_paths(std::env::split_paths(&path).chain([dir])).unwrap();
    launch.env("PATH", path);
}

/// Write the stub `zmx` into `dir` once. A restarted daemon reuses it: the
/// previous daemon's processes may still run it, and Linux refuses to write
/// to a running executable. The stub is renamed into place complete, so no
/// launch runs a partly written one.
fn write_zmx_stub(dir: &Path) {
    let zmx = dir.join("zmx");
    if zmx.is_file() {
        return;
    }
    fs::create_dir_all(dir).unwrap();
    let partial = dir.join("zmx.partial");
    fs::write(
        &partial,
        "#!/bin/sh\nif [ \"$1\" = list ]; then exit 0; fi\necho 'zmx is not installed on this test runner' >&2\nexit 127\n",
    )
    .unwrap();
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&partial, &zmx).unwrap();
}
