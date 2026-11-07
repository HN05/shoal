//! Wall-clock timestamps, distinct from monotonic durations and deadlines.
use std::{
    fmt,
    ops::{Add, Sub},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// A wall-clock time in whole Unix seconds, a JSON number and an SQLite
/// INTEGER on the wire and on disk.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct UnixTime(i64);

impl UnixTime {
    /// 1970-01-01 00:00:00 UTC.
    pub const EPOCH: Self = Self(0);
    /// The latest representable time, for a deadline that never comes.
    pub const MAX: Self = Self(i64::MAX);

    /// The current time, or the epoch when the clock is before it.
    pub fn now() -> Self {
        Self::from_duration(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default(),
        )
    }

    pub const fn from_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    /// The whole seconds of a time since the epoch, saturating at [`Self::MAX`].
    pub fn from_duration(since_epoch: Duration) -> Self {
        Self::EPOCH + since_epoch
    }

    /// The whole seconds of `time`, or the epoch for a time before it.
    pub fn from_system_time(time: SystemTime) -> Self {
        Self::from_duration(time.duration_since(UNIX_EPOCH).unwrap_or_default())
    }

    pub const fn seconds(self) -> i64 {
        self.0
    }

    /// How long after `earlier` this is, or zero when it is not later.
    pub fn duration_since(self, earlier: Self) -> Duration {
        Duration::from_secs(self.0.saturating_sub(earlier.0).try_into().unwrap_or(0))
    }

    /// `YYYY-MM-DD HH:MM` in the local time zone.
    pub fn local_minutes(self) -> String {
        self.local(false)
    }

    /// `YYYY-MM-DD HH:MM:SS` in the local time zone.
    pub fn local_seconds(self) -> String {
        self.local(true)
    }

    /// When this happened, read at `now`: `HH:MM` on the same local day,
    /// otherwise the whole days, weeks, months or years since that day, such
    /// as `1 day ago` or `3 weeks ago`. A time after `now` shows its time.
    pub fn local_ago(self, now: Self) -> String {
        let (Some(then), Some(today)) = (self.local_tm(), now.local_tm()) else {
            return self.to_string();
        };
        match civil_day(&today) - civil_day(&then) {
            ..=0 => format!("{:02}:{:02}", then.tm_hour, then.tm_min),
            days => days_ago(days),
        }
    }

    fn local(self, with_seconds: bool) -> String {
        let Some(tm) = self.local_tm() else {
            return self.to_string();
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
    fn local_tm(self) -> Option<libc::tm> {
        // Infer the ABI type from localtime_r; libc deprecates its musl time_t alias.
        let time = self.0 as _;
        // SAFETY: localtime_r writes only into the zeroed `tm` passed to it.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            (!libc::localtime_r(&time, &mut tm).is_null()).then_some(tm)
        }
    }
}

/// Whole seconds later, saturating at [`UnixTime::MAX`].
impl Add<Duration> for UnixTime {
    type Output = Self;

    fn add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(whole_seconds(duration)))
    }
}

/// Whole seconds earlier, saturating at the earliest representable time.
impl Sub<Duration> for UnixTime {
    type Output = Self;

    fn sub(self, duration: Duration) -> Self {
        Self(self.0.saturating_sub(whole_seconds(duration)))
    }
}

/// The seconds as a number, as a fallback where no local time can be shown.
impl fmt::Display for UnixTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for UnixTime {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for UnixTime {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

/// Whole Unix seconds, or zero when the clock is before the epoch.
pub fn unix_seconds() -> u64 {
    UnixTime::now().seconds() as u64
}

/// `YYYY-MM-DD HH:MM` in the local time zone.
pub fn local_minutes(unix_seconds: i64) -> String {
    UnixTime(unix_seconds).local_minutes()
}

/// `YYYY-MM-DD HH:MM:SS` in the local time zone.
pub fn local_seconds(unix_seconds: i64) -> String {
    UnixTime(unix_seconds).local_seconds()
}

/// See [`UnixTime::local_ago`].
pub fn local_ago(unix_seconds: i64, now: i64) -> String {
    UnixTime(unix_seconds).local_ago(UnixTime(now))
}

fn whole_seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
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
