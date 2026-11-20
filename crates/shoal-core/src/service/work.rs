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
