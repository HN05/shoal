//! The slice Shoal's work runs in on Linux, [`SLICE`]: an execution
//! wrapper starts the command of an agent, a CI job, a demo or a setup in a
//! scope of its own there, wherever the wrapper runs, such as in a terminal
//! host's place, so the command keeps that terminal while the slice holds
//! its memory. The daemon, places, workers and wrappers stay out of it.
//! Where no scope can start, as without a user bus or before systemd 254,
//! and in a build that leaves the user's files to the installed Shoal, the
//! command starts directly.
use std::{ffi::OsString, path::PathBuf};

use tokio::sync::OnceCell;

/// The slice of the user's service manager Shoal's work runs in, which
/// systemd nests in `shoal.slice`.
const SLICE: &str = "shoal-work.slice";

/// The launcher [`launcher`] found, unset until it first runs.
static LAUNCHER: OnceCell<Vec<OsString>> = OnceCell::const_new();

/// The command a work command starts through to run in a scope of its own
/// in [`SLICE`]; empty, which starts it directly, where no scope can start.
/// The first call in a process finds it, and later ones return the same.
pub async fn launcher() -> &'static [OsString] {
    LAUNCHER
        .get_or_init(|| async {
            if !crate::build::changes_user_files() {
                return Vec::new();
            }
            (super::scope::launcher(Some(SLICE)).await).unwrap_or_default()
        })
        .await
}

/// `command`, whose environment and directory are set, started through
/// `launcher` instead, which [`launcher`] gives: with the same arguments,
/// environment and directory, and the bus variables its environment sets
/// given back to it after the launcher reached the user's service manager
/// with this process's. Its standard streams and hooks are set after.
pub fn launched(
    command: tokio::process::Command,
    launcher: &[OsString],
) -> tokio::process::Command {
    if launcher.is_empty() {
        return command;
    }
    let own = command.as_std();
    let set = |name: &str| {
        (own.get_envs())
            .find(|(variable, _)| *variable == name)
            .map(|(_, value)| value.map(ToOwned::to_owned))
    };
    let restore = super::scope::restore_bus(launcher, set);
    let mut argv = launcher.iter().chain(&restore);
    let mut through = tokio::process::Command::new(argv.next().expect("a launcher has a program"));
    through
        .args(argv)
        .arg(own.get_program())
        .args(own.get_args());
    for (name, value) in own.get_envs() {
        match value {
            Some(value) => through.env(name, value),
            None => through.env_remove(name),
        };
    }
    if let Some(directory) = own.get_current_dir() {
        through.current_dir(directory);
    }
    through
}

/// The slice's control group, where the user's service manager keeps it;
/// `None` in a build that leaves the slice to the installed Shoal, whose
/// work runs there.
pub fn control_group() -> Option<PathBuf> {
    if !crate::build::changes_user_files() {
        return None;
    }
    let own = std::fs::read_to_string("/proc/self/cgroup").ok();
    // SAFETY: getuid has no arguments or memory-safety preconditions.
    let manager = manager_group(own.as_deref(), unsafe { libc::getuid() });
    Some(
        PathBuf::from("/sys/fs/cgroup")
            .join(manager)
            .join("shoal.slice")
            .join(SLICE),
    )
}

/// Whether the process `pid` runs in the slice; false once it is gone.
pub fn holds(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|groups| in_slice(&groups))
}

/// Whether the control groups `groups` of a process place it in the slice.
fn in_slice(groups: &str) -> bool {
    let slice = format!("/shoal.slice/{SLICE}/");
    (groups.lines())
        .filter_map(|line| line.strip_prefix("0::"))
        .any(|path| path.contains(&slice))
}

/// The control group of user `uid`'s service manager, relative to the
/// hierarchy's root: the part of `own`, this process's control groups, up
/// to it where this process runs below it, as in a terminal or a service,
/// and otherwise where systemd keeps it.
fn manager_group(own: Option<&str>, uid: u32) -> String {
    let manager = format!("user@{uid}.service");
    let below = own
        .and_then(|own| own.lines().find_map(|line| line.strip_prefix("0::/")))
        .and_then(|path| Some(&path[..path.find(&manager)? + manager.len()]));
    below.map_or_else(
        || format!("user.slice/user-{uid}.slice/{manager}"),
        str::to_owned,
    )
}

/// What the slice's memory may reach: above `high` the kernel throttles
/// and reclaims its work, and above `max` its OOM killer kills a process of
/// it, inside the slice only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub high: u64,
    pub max: u64,
}

impl Cap {
    /// The cap of a `limit` in bytes: throttled from the limit, killed only
    /// a twentieth above it, so throttling keeps work below the limit and
    /// kills are a last resort.
    pub fn of(limit: u64) -> Self {
        Self {
            high: limit,
            max: limit.saturating_add(limit / 20),
        }
    }
}

/// Cap the slice's memory at `cap`, or lift its cap without one, unless it
/// has that cap already, until the next boot or change: the user's service
/// manager keeps it for the slice while nothing runs there, so work started
/// later takes it too.
pub async fn cap_memory(cap: Option<Cap>) -> anyhow::Result<()> {
    use super::control::{command, query};
    use crate::tools::Tool;
    let properties = properties(cap);
    let current = query(
        Tool::Systemctl,
        &["--user", "show", "--property=MemoryHigh,MemoryMax", SLICE],
    )
    .await?;
    if properties
        .iter()
        .all(|property| current.lines().any(|line| line == property))
    {
        return Ok(());
    }
    let mut args = vec!["--user", "set-property", "--runtime", SLICE];
    args.extend(properties.iter().map(String::as_str));
    command(Tool::Systemctl, &args, true).await?;
    Ok(())
}

/// The slice's properties that give it `cap`, as `systemctl show` prints
/// them and `set-property` takes them.
fn properties(cap: Option<Cap>) -> [String; 2] {
    let value = |bytes: Option<u64>| bytes.map_or_else(|| "infinity".to_owned(), |b| b.to_string());
    [
        format!("MemoryHigh={}", value(cap.map(|cap| cap.high))),
        format!("MemoryMax={}", value(cap.map(|cap| cap.max))),
    ]
}

#[cfg(test)]
mod tests;
