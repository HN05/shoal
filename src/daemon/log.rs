//! Daemon log lines on stderr, which the service manager appends to a file.
use std::{io::Write, time::Duration};

use tokio::time::Instant;

/// Work running this long is logged, once, and again when it ends.
pub(crate) const SLOW: Duration = Duration::from_secs(60);

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

/// Run `work`, logging it once it has run for [`SLOW`] and again when it
/// ends, so work that hangs leaves a trace. `what` names it, such as
/// `request status`.
pub(crate) async fn when_slow<T>(what: &str, work: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let mut work = std::pin::pin!(work);
    tokio::select! {
        output = &mut work => return output,
        () = tokio::time::sleep(SLOW) => log!("{what} still running after {}", elapsed(SLOW)),
    }
    let output = work.await;
    log!("{what} finished after {}", elapsed(started.elapsed()));
    output
}

/// A duration in its two largest units, such as `2h 3m` or `45s`.
pub(crate) fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

#[cfg(test)]
mod tests;
