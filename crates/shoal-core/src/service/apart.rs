//! Starting a process apart from the systemd service this process runs as,
//! so that systemd's stop of that service, as once the daemon exited with a
//! failure, leaves it running: systemd ends every process in a service's
//! control group, and a transient scope of its own, in the service's slice,
//! keeps the process out of it. launchd ends only the process group of a
//! job whose program exited, which a process in a session of its own has
//! already left.
use std::ffi::OsString;

use super::control::{command, query};
use crate::tools::Tool;

/// The first systemd whose `systemd-run` takes `--expand-environment`, which
/// otherwise expands `$` in a scope's arguments.
const UNEXPANDED_ARGUMENTS: u32 = 254;

/// The command that starts a program apart from the systemd user service
/// this process runs as, in the same slice; empty where this process runs as
/// no such service, where `systemd-run` would change the program's
/// arguments, or where it cannot start a scope, as without a user bus.
pub async fn launcher() -> Vec<OsString> {
    let Some(service) = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .as_deref()
        .and_then(Service::of)
    else {
        return Vec::new();
    };
    match query(Tool::SystemdRun, &["--version"]).await {
        Ok(version) if keeps_arguments(&version) => {}
        _ => return Vec::new(),
    }
    let launcher = service.launcher();
    if starts_scopes(&launcher).await {
        launcher
    } else {
        Vec::new()
    }
}

/// Whether `launcher` starts a program, which `true` tries once.
async fn starts_scopes(launcher: &[OsString]) -> bool {
    let Some(mut args) = (launcher.iter().skip(1))
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<&str>>>()
    else {
        return false;
    };
    args.push("true");
    command(Tool::SystemdRun, &args, false)
        .await
        .unwrap_or(false)
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

    /// The `systemd-run` command that starts a program in a scope of its
    /// own in this service's slice, which goes once its processes have.
    fn launcher(&self) -> Vec<OsString> {
        let mut launcher: Vec<OsString> = [
            Tool::SystemdRun.program(),
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if let Some(slice) = &self.slice {
            launcher.push(format!("--slice={slice}").into());
        }
        launcher.push("--".into());
        launcher
    }
}

/// Whether the systemd that `systemd-run --version` printed `version` for
/// passes a scope's arguments on unchanged.
fn keeps_arguments(version: &str) -> bool {
    version
        .split_whitespace()
        .nth(1)
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number >= UNEXPANDED_ARGUMENTS)
}

#[cfg(test)]
mod tests;
