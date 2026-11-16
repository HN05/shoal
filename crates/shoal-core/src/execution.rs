//! Tracked executions as the daemon and the wrapper share them: the agent a
//! wrapper runs, launching one detached, its recovery records, and the
//! terminal it hands its command. The wrapper itself is `shoal-workers`'.
mod copy_ignored;
mod detached;
pub mod recovery;
mod terminal;

use std::{os::unix::process::ExitStatusExt as _, process::ExitStatus};

pub use copy_ignored::copy_ignored_worker;
pub use detached::{DetachedExecution, DetachedLaunch, Wrapper, launch_detached, report_launch};
pub(crate) use terminal::reset_input_modes;
pub use terminal::{Terminal, clear_terminal, run_in_terminal, take_foreground};

/// An agent a tracked wrapper runs: its name, as `--agent` names it, the
/// model and effort it runs with, and the mode it started in, which its
/// restores keep.
#[derive(Debug, Clone)]
pub struct TrackedAgent {
    pub name: crate::agent::Agent,
    pub model: crate::agent::ModelChoice,
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
}

#[cfg(any(test, feature = "test-support"))]
impl TrackedAgent {
    /// Agent `name` running with the tool's own model and effort.
    pub fn named(name: impl Into<crate::agent::Agent>) -> Self {
        Self {
            narrowing: Default::default(),
            name: name.into(),
            model: Default::default(),
            mode: crate::config::modes::ModeName::DEFAULT,
        }
    }
}

/// Exit status as a shell would report it: the code, or 128 + signal.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}
