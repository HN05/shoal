//! Fixture homes: temporary directories named for the test process that
//! made them, removed with what still runs in them when dropped, or once
//! that process is gone, as when a signal ends a test before any drop runs,
//! by the watcher it started or a later test.
use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Once,
    time::{Duration, Instant},
};

use tempfile::TempDir;

/// Where homes go, outside a runner's `TMPDIR`, which can make socket
/// paths in them too long.
const PARENT: &str = "/tmp";

/// A fixture home, `/tmp/.t<pid>.<random>` after the test process that
/// owns it: short, since socket paths in it are near their limit.
pub struct Home {
    dir: TempDir,
}

impl Home {
    /// A new home, after removing those whose test processes are gone, with
    /// this process's watcher started.
    pub fn new() -> Self {
        static SWEEP: Once = Once::new();
        SWEEP.call_once(|| {
            remove_abandoned();
            watch_this_process();
        });
        let dir = tempfile::Builder::new()
            .prefix(&format!(".t{}.", std::process::id()))
            .rand_bytes(4)
            .tempdir_in(PARENT)
            .unwrap();
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// The processes a fixture's commands leave in its home, such as the agent
/// of a `shoal start` a test killed, place watchers, zmx workers and a
/// demo's server, many in sessions of their own whose parents exited, stop
/// before it goes: one still running would never end, and one writing into
/// it while it is removed would keep it.
impl Drop for Home {
    fn drop(&mut self) {
        stop_processes_in(&[self.path().to_owned()]);
    }
}

/// The variable naming the test process whose homes [`sweep_after_exit`]
/// removes.
const WATCHED: &str = "SHOAL_TEST_HOMES_OF";

/// Start this test binary's [`sweep_after_exit`] as this process's watcher,
/// which holds the read end of a pipe whose write end this process keeps
/// until it exits, however it exits: a signal that ends the test before its
/// drops run then leaves nothing running in its homes, even when no later
/// test sweeps them. The watcher has a session of its own, which the
/// signals for the test's process group or terminal do not reach.
fn watch_this_process() {
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{module}::sweep_after_exit"),
            "--ignored",
        ])
        .env(WATCHED, std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    in_own_session(&mut command);
    // The watcher, never waited for, outlives this process, and no other
    // child inherits the write end, which closes on exec.
    std::mem::forget(command.spawn().unwrap());
}

/// Start `command` in a session of its own.
fn in_own_session(command: &mut Command) {
    // SAFETY: setsid only changes the child's session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The watcher [`watch_this_process`] starts: once its input ends, as the
/// test process that keeps the other end exits, stop what runs in that
/// process's homes and remove them.
#[test]
#[ignore = "the watcher each test process starts for its homes"]
fn sweep_after_exit() {
    let Some(watched) = std::env::var(WATCHED)
        .ok()
        .and_then(|pid| pid.parse::<i32>().ok())
    else {
        return;
    };
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    remove_homes(|owner| owner == watched);
}

/// Stop the processes of the homes whose test processes are gone, and
/// remove the homes. Another test may sweep the same ones meanwhile.
pub fn remove_abandoned() {
    remove_homes(gone);
}

/// Stop the processes of the homes whose owners `owned` selects, and remove
/// the homes.
fn remove_homes(owned: impl Fn(i32) -> bool) {
    let Ok(entries) = fs::read_dir(PARENT) else {
        return;
    };
    let homes: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| owner(&entry.file_name().to_string_lossy()).is_some_and(&owned))
        .map(|entry| entry.path())
        .collect();
    if homes.is_empty() {
        return;
    }
    stop_processes_in(&homes);
    for home in homes {
        let _ = fs::remove_dir_all(home);
    }
}

fn owner(name: &str) -> Option<i32> {
    let (pid, _) = name.strip_prefix(".t")?.split_once('.')?;
    let pid: u32 = pid.parse().ok().filter(|pid| *pid > 0)?;
    pid.try_into().ok()
}

/// Whether process `pid` no longer exists. A process that took over its ID
/// keeps its home until it too is gone.
fn gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let refused = unsafe { libc::kill(pid, 0) } != 0;
    refused && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Kill, with their groups, the processes that work in one of `homes` or
/// name one on their command line, until none is left or ten seconds pass.
fn stop_processes_in(homes: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found = processes_in(homes);
        if found.is_empty() || Instant::now() >= deadline {
            break;
        }
        for pid in found {
            // SAFETY: getpgid and kill only read and signal processes.
            let target = if unsafe { libc::getpgid(pid) } == pid {
                -pid
            } else {
                pid
            };
            unsafe { libc::kill(target, libc::SIGKILL) };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The IDs of the processes, other than zombies and this one, that work in
/// one of `homes` or name a path in one on their command line.
fn processes_in(homes: &[PathBuf]) -> Vec<i32> {
    let named: Vec<String> = (homes.iter())
        .map(|home| format!("{}/", home.display()))
        .collect();
    let mut found = processes_naming(&named);
    // The kernel reports working directories with symbolic links resolved.
    let homes: Vec<PathBuf> = (homes.iter())
        .filter_map(|home| fs::canonicalize(home).ok())
        .collect();
    // A scan that fails leaves its processes rather than panicking in a drop.
    let working = shoal_core::test_support::working_directories().unwrap_or_default();
    found.extend(
        (working.into_iter())
            .filter(|(_, directory)| homes.iter().any(|home| directory.starts_with(home)))
            .map(|(pid, _)| pid as i32),
    );
    found
}

/// The IDs of the processes whose command line contains one of `texts`,
/// other than zombies.
fn processes_naming(texts: &[String]) -> Vec<i32> {
    let Ok(output) = Command::new("ps")
        .args(["-ax", "-ww", "-o", "pid=,stat=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| texts.iter().any(|text| line.contains(text.as_str())))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let stat = fields.next()?;
            (!stat.starts_with('Z')).then_some(pid)
        })
        .collect()
}
