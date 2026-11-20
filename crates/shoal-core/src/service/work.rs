//! The slice Shoal's work runs in on Linux, [`SLICE`]: an execution
//! wrapper starts the command of an agent, a CI job, a demo or a setup in a
//! scope of its own there, wherever the wrapper runs, such as in a terminal
//! host's place, so the command keeps that terminal while the slice holds
//! its memory. The daemon, places, workers and wrappers stay out of it.
//! Where no scope can start, as without a user bus or before systemd 254,
//! and in a build that leaves the user's files to the installed Shoal, the
//! command starts directly.
use std::ffi::OsString;

use tokio::sync::OnceCell;

/// The slice of the user's service manager Shoal's work runs in, which
/// systemd nests in `shoal.slice`.
pub const SLICE: &str = "shoal-work.slice";

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
