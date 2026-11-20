//! Starting a process apart from the systemd service this process runs as,
//! so that systemd's stop of that service, as once the daemon exited with a
//! failure, leaves it running: systemd ends every process in a service's
//! control group, and a transient scope of its own, in the service's slice,
//! keeps the process out of it. launchd ends only the process group of a
//! job whose program exited, which a process in a session of its own has
//! already left.
use std::{ffi::OsString, sync::OnceLock};

/// The launcher [`start_apart_from_service`] found, unset until it runs.
static LAUNCHER: OnceLock<Vec<OsString>> = OnceLock::new();

/// Start every process this process starts in a session of its own apart
/// from the systemd user service it runs as, from now on, as a daemon its
/// service manager started does.
pub async fn start_apart_from_service() {
    remember(launcher().await);
}

/// Start processes through `launcher` from now on, unless a launcher was
/// already chosen.
pub(crate) fn remember(launcher: Vec<OsString>) {
    let _ = LAUNCHER.set(launcher);
}

/// The command a process this process starts in a session of its own
/// starts through; empty, which starts it directly, until
/// [`start_apart_from_service`] runs, as in the CLI, and where it found no
/// launcher.
pub(crate) fn current() -> &'static [OsString] {
    LAUNCHER.get().map_or(&[], Vec::as_slice)
}

/// The command that starts a program apart from the systemd user service
/// this process runs as, in the same slice; empty where this process runs as
/// no such service, or where no scope can start, as without a user bus.
async fn launcher() -> Vec<OsString> {
    let Some(service) = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .as_deref()
        .and_then(Service::of)
    else {
        return Vec::new();
    };
    (super::scope::launcher(service.slice.as_deref()).await).unwrap_or_default()
}

/// The systemd user service of a control group.
#[derive(Debug, PartialEq, Eq)]
struct Service {
    /// The slice it runs in, when it runs in one.
    slice: Option<String>,
}

impl Service {
    /// The user service of the control group `/proc/<pid>/cgroup` lists on
    /// the unified hierarchy.
    fn of(cgroup: &str) -> Option<Self> {
        let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
        if !path.split('/').any(|unit| unit.starts_with("user@")) {
            return None;
        }
        let mut units = path.rsplit('/');
        units.next().filter(|unit| unit.ends_with(".service"))?;
        let slice = units.next().filter(|unit| unit.ends_with(".slice"));
        Some(Self {
            slice: slice.map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests;
