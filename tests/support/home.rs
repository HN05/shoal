//! Fixture homes: temporary directories named for the test process that
//! made them, removed with what still runs in them when dropped, or by a
//! later test once that process is gone, as when a signal ends a test
//! before any drop runs.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
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
    /// A new home, after removing those whose test processes are gone.
    pub fn new() -> Self {
        static SWEEP: Once = Once::new();
        SWEEP.call_once(remove_abandoned);
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

/// Stop the processes of the homes whose test processes are gone, and
/// remove the homes. Another test may sweep the same ones meanwhile.
pub fn remove_abandoned() {
    let Ok(entries) = fs::read_dir(PARENT) else {
        return;
    };
    let abandoned: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| owner(&entry.file_name().to_string_lossy()).is_some_and(gone))
        .map(|entry| entry.path())
        .collect();
    if abandoned.is_empty() {
        return;
    }
    stop_processes_in(&abandoned);
    for home in abandoned {
        let _ = fs::remove_dir_all(home);
    }
}

/// The ID of the test process a home's directory `name` names.
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
