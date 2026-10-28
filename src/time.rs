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

/// When something happened, read at `now`: `HH:MM` on the same local day,
/// otherwise the whole days, weeks, months or years since that day, such as
/// `1 day ago` or `3 weeks ago`. A time after `now` shows its time.
pub(crate) fn local_ago(unix_seconds: i64, now: i64) -> String {
    let (Some(then), Some(today)) = (local_tm(unix_seconds), local_tm(now)) else {
        return unix_seconds.to_string();
    };
    match civil_day(&today) - civil_day(&then) {
        ..=0 => format!("{:02}:{:02}", then.tm_hour, then.tm_min),
        days => days_ago(days),
    }
}

/// A positive number of calendar days in the largest whole unit.
fn days_ago(days: i64) -> String {
    let (count, unit) = match days {
        ..7 => (days, "day"),
        7..30 => (days / 7, "week"),
        30..365 => (days / 30, "month"),
        _ => (days / 365, "year"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

fn local(unix_seconds: i64, with_seconds: bool) -> String {
    let Some(tm) = local_tm(unix_seconds) else {
        return unix_seconds.to_string();
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

/// The broken-down local time, or `None` when libc cannot convert it.
fn local_tm(unix_seconds: i64) -> Option<libc::tm> {
    // Infer the ABI type from localtime_r; libc deprecates its musl time_t alias.
    let time = unix_seconds as _;
    // SAFETY: localtime_r writes only into the zeroed `tm` passed to it.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&time, &mut tm).is_null()).then_some(tm)
    }
}

/// Days from 1970-01-01 to the calendar date of `tm`, so two dates subtract
/// to the number of midnights between them whatever the UTC offset.
fn civil_day(tm: &libc::tm) -> i64 {
    // Howard Hinnant's days_from_civil, with years starting in March.
    let month = i64::from(tm.tm_mon) + 1;
    let year = i64::from(tm.tm_year) + 1900 - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year =
        (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(tm.tm_mday) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests;
