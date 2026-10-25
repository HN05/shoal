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

    #[test]
    fn calendar_days_count_from_the_epoch() {
        // SAFETY: libc::tm holds integers and a pointer, all valid when zeroed.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        (tm.tm_year, tm.tm_mon, tm.tm_mday) = (70, 0, 1);
        assert_eq!(super::civil_day(&tm), 0);
        (tm.tm_year, tm.tm_mon, tm.tm_mday) = (100, 2, 1);
        assert_eq!(super::civil_day(&tm), 11_017);
        (tm.tm_year, tm.tm_mon, tm.tm_mday) = (126, 9, 26);
        assert_eq!(super::civil_day(&tm), 20_752);
    }

    #[test]
    fn same_day_shows_the_time_and_earlier_days_how_long_ago() {
        // Noon local time keeps whole-day steps clear of midnight.
        let noon = noon_today();
        let day = 86_400;
        assert_eq!(
            super::local_ago(noon, noon + 60),
            &super::local_minutes(noon)[11..]
        );
        assert_eq!(
            super::local_ago(noon + 60, noon),
            &super::local_minutes(noon + 60)[11..]
        );
        for (days, text) in [
            (1, "1 day ago"),
            (6, "6 days ago"),
            (7, "1 week ago"),
            (29, "4 weeks ago"),
            (30, "1 month ago"),
            (364, "12 months ago"),
            (365, "1 year ago"),
            (800, "2 years ago"),
        ] {
            assert_eq!(super::local_ago(noon - days * day, noon), text, "{days}");
        }
    }

    /// About noon today in the local time zone.
    fn noon_today() -> i64 {
        let now = super::unix_seconds() as i64;
        let Some(tm) = super::local_tm(now) else {
            return now;
        };
        let since_midnight = i64::from(tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec);
        now - since_midnight + 12 * 3600
    }
}
