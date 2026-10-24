//! Standard output for command results. Rust ignores SIGPIPE, so a reader that
//! closes early, such as `head`, turns the next write into an error that
//! `println!` panics on. These writes end the process quietly instead, with the
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

pub fn print(arguments: std::fmt::Arguments<'_>) {
    if let Err(error) = write(|stdout| stdout.write_fmt(arguments)) {
        panic!("failed printing to stdout: {error}");
    }
}

/// Write to the locked stdout, ending the process when its reader has gone.
pub fn write<T>(write: impl FnOnce(&mut StdoutLock<'static>) -> io::Result<T>) -> io::Result<T> {
    let result = write(&mut io::stdout().lock());
    if result
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    {
        std::process::exit(CLOSED_STATUS);
    }
    result
}

/// Flush stdout, ending the process when its reader has gone.
pub fn flush() -> io::Result<()> {
    write(|stdout| stdout.flush())
}
