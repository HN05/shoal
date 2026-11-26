//! The order Linux's OOM killer takes processes in, which Shoal sets from
//! its stop order. The kernel kills the process whose share of memory,
//! raised by its `oom_score_adj`, is largest, so the work Shoal would stop
//! first gets the highest score, and the daemon, places and workers keep
//! the floor the user's service manager allows them without privilege.
//! Within one score the kernel still takes the largest process first.
//! Other systems have no such score, and these do nothing there.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The score of the first entry of the stop order.
pub(crate) const FIRST: i16 = 900;
/// The score of the last entry, which the command an execution's wrapper
/// starts takes from its start, until the daemon gives it its place.
pub(crate) const LAST: i16 = 200;

/// [`LAST`] as written to a score file, which a child writes before exec
/// without formatting it.
#[cfg(any(target_os = "linux", test))]
const START_SCORE: &[u8] = b"200";

/// The scores of `count` entries of the stop order, first first: [`FIRST`]
/// to [`LAST`], linearly between.
pub fn scores(count: usize) -> Vec<i16> {
    let span = i64::from(FIRST - LAST);
    let steps = i64::try_from(count.saturating_sub(1))
        .unwrap_or(i64::MAX)
        .max(1);
    (0..count)
        .map(|place| {
            let place = i64::try_from(place).unwrap_or(i64::MAX);
            // Lies between LAST and FIRST, so it fits.
            (i64::from(FIRST) - span * place / steps) as i16
        })
        .collect()
}

/// Lower this process's score to that of the user's service manager that
/// started it, the lowest it may set without privilege, returning that
/// score; `None` where no service manager started it.
pub fn lower_to_manager() -> Result<Option<i16>> {
    let Some(manager) = std::env::var_os("MANAGERPID") else {
        return Ok(None);
    };
    let manager: u32 = (manager.to_str().and_then(|pid| pid.parse().ok()))
        .context("MANAGERPID is not a process ID")?;
    let floor = read(manager)?;
    write(&score_path("self"), floor)?;
    Ok(Some(floor))
}

/// This process's score, the floor of the places and workers it starts.
pub fn own_score() -> Result<i16> {
    read_path(&score_path("self"))
}

/// Set the score of the process `pid` to `score` unless it has it already,
/// returning whether it wrote.
pub fn set(pid: u32, score: i16) -> Result<bool> {
    if read(pid)? == score {
        return Ok(false);
    }
    write(&score_path(&pid.to_string()), score)?;
    Ok(true)
}

/// Set the score of the process `pid` to `score`, as [`set`] does, while
/// it runs in the control group `scope`, where it was listed, so a PID
/// reused since then takes no score.
pub fn set_in_scope(pid: u32, scope: &Path, score: i16) -> Result<bool> {
    let groups = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .with_context(|| format!("read the control group of process {pid}"))?;
    let group = unified(&groups).map(|path| Path::new("/sys/fs/cgroup").join(path));
    if group.as_deref() != Some(scope) {
        return Ok(false);
    }
    set(pid, score)
}

/// Have the command `command` starts take [`LAST`] before it runs, so
/// what it starts inherits a score above Shoal's own. A failed write
/// leaves it the wrapper's, for the daemon to correct.
#[cfg(target_os = "linux")]
pub fn start_at_last(command: &mut tokio::process::Command) {
    // SAFETY: the hook makes only async-signal-safe calls on a static path
    // and buffer, and ignores their failures.
    unsafe {
        command.pre_exec(|| {
            let file = libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY);
            if file >= 0 {
                libc::write(file, START_SCORE.as_ptr().cast(), START_SCORE.len());
                libc::close(file);
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn start_at_last(_command: &mut tokio::process::Command) {}

/// The control group of the transient scope the process `pid` runs in, as
/// Shoal's launcher starts a detached command in one of its own; `None`
/// where it runs in another kind of group, or in this process's own, as a
/// command does that a daemon outside a service started directly.
pub fn scope_of(pid: u32) -> Result<Option<PathBuf>> {
    let read = |process: &str| {
        let path = Path::new("/proc").join(process).join("cgroup");
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
    };
    let (groups, own) = (read(&pid.to_string())?, read("self")?);
    Ok(own_scope(&groups, &own).map(|path| Path::new("/sys/fs/cgroup").join(path)))
}

/// The control group the process `pid` runs in on the unified hierarchy,
/// whatever kind it is.
pub fn group_of(pid: u32) -> Result<Option<PathBuf>> {
    let path = Path::new("/proc").join(pid.to_string()).join("cgroup");
    let groups =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(unified(&groups).map(|path| Path::new("/sys/fs/cgroup").join(path)))
}

/// The transient scope the control groups `groups` of a process name, unless
/// the groups `own` of this process name the same group.
fn own_scope<'a>(groups: &'a str, own: &str) -> Option<&'a str> {
    transient_scope(groups).filter(|scope| unified(own) != Some(*scope))
}

/// The path of the transient scope the control groups `groups` of a
/// process name on the unified hierarchy, as `systemd-run --scope` names
/// one, or Shoal one for a space's work, relative to the hierarchy's root.
fn transient_scope(groups: &str) -> Option<&str> {
    let path = unified(groups)?;
    let unit = path.rsplit('/').next()?;
    let transient = ["run-", crate::service::work::SCOPE_PREFIX]
        .iter()
        .any(|prefix| unit.starts_with(prefix));
    (transient && unit.ends_with(".scope")).then_some(path)
}

/// The group the control groups `groups` name on the unified hierarchy,
/// relative to its root.
fn unified(groups: &str) -> Option<&str> {
    groups.lines().find_map(|line| line.strip_prefix("0::/"))
}

pub fn scope_processes(scope: &Path) -> Result<Vec<u32>> {
    let listed = std::fs::read_to_string(scope.join("cgroup.procs"))
        .with_context(|| format!("list the processes of {}", scope.display()))?;
    Ok(listed.lines().filter_map(|pid| pid.parse().ok()).collect())
}

/// Have the kernel's kill of one process of the control group `scope` end
/// every process in it.
pub fn kill_as_group(scope: &Path) -> Result<()> {
    let path = scope.join("memory.oom.group");
    if std::fs::read_to_string(&path).is_ok_and(|grouped| grouped.trim() == "1") {
        return Ok(());
    }
    std::fs::write(&path, "1").with_context(|| format!("write {}", path.display()))
}

fn score_path(process: &str) -> PathBuf {
    Path::new("/proc").join(process).join("oom_score_adj")
}

fn read(pid: u32) -> Result<i16> {
    read_path(&score_path(&pid.to_string()))
}

fn read_path(path: &Path) -> Result<i16> {
    let score =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    (score.trim().parse()).with_context(|| format!("{} is not a score", path.display()))
}

fn write(path: &Path, score: i16) -> Result<()> {
    std::fs::write(path, score.to_string()).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests;
