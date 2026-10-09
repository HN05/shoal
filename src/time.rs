//! Wall-clock timestamps, distinct from monotonic durations and deadlines.
use std::time::{SystemTime, UNIX_EPOCH};

/// Whole Unix seconds, or zero when the clock is before the epoch.
pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `YYYY-MM-DD HH:MM` in the local time zone.
pub(crate) fn local_minutes(unix_seconds: i64) -> String {
    local(unix_seconds, false)
}

/// `YYYY-MM-DD HH:MM:SS` in the local time zone.
pub(crate) fn local_seconds(unix_seconds: i64) -> String {
    local(unix_seconds, true)
}

fn local(unix_seconds: i64, with_seconds: bool) -> String {
    // Infer the ABI type from localtime_r; libc deprecates its musl time_t alias.
    let time = unix_seconds as _;
    // SAFETY: localtime_r writes only into the zeroed `tm` passed to it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return unix_seconds.to_string();
        }
        tm
    };
    let minutes = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    );
    if with_seconds {
        format!("{minutes}:{:02}", tm.tm_sec)
    } else {
        minutes
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_times_use_a_fixed_width_date_and_time() {
        // Check the shape without assuming the process time zone.
        let text = super::local_minutes(1_700_000_000);
        assert_eq!(text.len(), 16, "{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
        let text = super::local_seconds(1_700_000_000);
        assert_eq!(text.len(), 19, "{text}");
        assert_eq!(&text[16..17], ":");
    }
}
