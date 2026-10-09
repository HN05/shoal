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
mod tests {
    use std::io::{self, Write};

    struct Full;

    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lines_are_timestamped_and_failed_writes_are_dropped() {
        let mut out = Vec::new();
        super::line(&mut out, format_args!("auto cleanup: {}", "failed"));
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with(" auto cleanup: failed\n"), "{text}");
        assert_eq!(text.find(' '), Some(10), "{text}");
        super::line(&mut Full, format_args!("disk full"));
    }
}
