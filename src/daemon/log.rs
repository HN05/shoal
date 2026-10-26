//! Daemon log lines on stderr, which the service manager appends to a file.
use std::io::Write;

/// Write one timestamped line to the daemon log. A failed write is dropped:
/// a full disk must not end the task that logs, as `eprintln!` would.
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::daemon::log::line(&mut std::io::stderr().lock(), format_args!($($arg)*))
    };
}
pub(crate) use log;

pub(crate) fn line(out: &mut impl Write, message: std::fmt::Arguments<'_>) {
    let now = crate::time::local_seconds(crate::time::unix_seconds() as i64);
    // One write per line keeps concurrent tasks from interleaving within it.
    let _ = out.write_all(format!("{now} {message}\n").as_bytes());
}

#[cfg(test)]
mod tests;
