//! Standard output for command results. Rust ignores SIGPIPE, so a reader that
//! closes early, such as `head`, turns the next write into an error that
//! `println!` panics on. These writes end the command quietly instead, with the
//! status a shell reports for a process SIGPIPE ended, as `git log | head` does.

use std::io::{self, StdoutLock, Write};

/// 128 plus SIGPIPE's number, 13.
pub const CLOSED_STATUS: i32 = 141;

/// Print to stdout like `print!`, ending the process when its reader has gone.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::stdout::print(format_args!($($arg)*))
    };
}

/// Print a line to stdout like `println!`, ending the process when its reader
/// has gone.
macro_rules! outln {
    () => {
        $crate::stdout::print(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::stdout::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// A write that failed because stdout's reader has gone. The CLI ends with
/// [`CLOSED_STATUS`] and no message once the error has unwound its command,
/// so cleanup such as dropping a temporary export still runs.
#[derive(Debug)]
pub struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout closed")
    }
}

impl std::error::Error for StdoutClosed {}

/// The macros cannot return an error, so a closed reader ends the process here.
pub fn print(arguments: std::fmt::Arguments<'_>) {
    match io::stdout().lock().write_fmt(arguments) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            std::process::exit(CLOSED_STATUS)
        }
        Err(error) => panic!("failed printing to stdout: {error}"),
    }
}

/// Write to the locked stdout; a closed reader fails with [`StdoutClosed`].
pub fn write<T>(
    write: impl FnOnce(&mut StdoutLock<'static>) -> io::Result<T>,
) -> anyhow::Result<T> {
    write(&mut io::stdout().lock()).map_err(|error| match error.kind() {
        io::ErrorKind::BrokenPipe => StdoutClosed.into(),
        _ => error.into(),
    })
}

/// Flush stdout; a closed reader fails with [`StdoutClosed`].
pub fn flush() -> anyhow::Result<()> {
    write(|stdout| stdout.flush())
}
