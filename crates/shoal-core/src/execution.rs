//! Tracked executions as the daemon and the wrapper share them: launching
//! one detached, its recovery records, and the terminal it hands its
//! command. The wrapper itself is `shoal-workers`'.
mod copy_ignored;
mod detached;
pub mod recovery;
mod terminal;

use std::{os::unix::process::ExitStatusExt as _, process::ExitStatus};

pub use copy_ignored::copy_ignored_worker;
pub use detached::{DetachedExecution, DetachedLaunch, Wrapper, launch_detached, report_launch};
pub(crate) use terminal::reset_input_modes;
pub use terminal::{
    INPUT_MODES_RESET, Terminal, clear_terminal, run_in_terminal, take_foreground, terminal_size,
    terminal_width,
};

/// Exit status as a shell would report it: the code, or 128 + signal.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}
