use std::time::Duration;

use super::UnixTime;

#[test]
fn local_times_use_a_fixed_width_date_and_time() {
    // Check the shape without assuming the process time zone.
    let text = UnixTime::from_seconds(1_700_000_000).local_minutes();
    assert_eq!(text.len(), 16, "{text}");
    assert_eq!(&text[4..5], "-");
    assert_eq!(&text[10..11], " ");
    let text = UnixTime::from_seconds(1_700_000_000).local_seconds();
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
    let minute = Duration::from_secs(60);
    let day = Duration::from_secs(86_400);
    assert_eq!(noon.local_ago(noon + minute), &noon.local_minutes()[11..]);
    assert_eq!(
        (noon + minute).local_ago(noon),
        &(noon + minute).local_minutes()[11..]
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
        assert_eq!((noon - day * days).local_ago(noon), text, "{days}");
    }
}

#[test]
fn arithmetic_saturates_and_elapsed_time_is_never_negative() {
    let time = UnixTime::from_seconds(100);
    assert_eq!(
        time + Duration::from_millis(1_900),
        UnixTime::from_seconds(101)
    );
    assert_eq!(time - Duration::from_secs(150), UnixTime::from_seconds(-50));
    assert_eq!(UnixTime::MAX + Duration::from_secs(1), UnixTime::MAX);
    assert_eq!(time + Duration::MAX, UnixTime::MAX);
    assert_eq!(
        time.duration_since(UnixTime::from_seconds(40)),
        Duration::from_secs(60)
    );
    assert_eq!(time.duration_since(UnixTime::MAX), Duration::ZERO);
    assert_eq!(
        UnixTime::from_seconds(40).abs_diff(time),
        Duration::from_secs(60)
    );
}

#[test]
fn times_are_json_numbers_and_sqlite_integers() -> anyhow::Result<()> {
    let time = UnixTime::from_seconds(1_700_000_000);
    assert_eq!(serde_json::to_string(&time)?, "1700000000");
    assert_eq!(serde_json::from_str::<UnixTime>("1700000000")?, time);
    #[cfg(feature = "rusqlite")]
    {
        let db = rusqlite::Connection::open_in_memory()?;
        let (stored, kind): (UnixTime, String) =
            db.query_row("SELECT ?1, typeof(?1)", [time], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!((stored, kind.as_str()), (time, "integer"));
    }
    Ok(())
}

/// About noon today in the local time zone.
fn noon_today() -> UnixTime {
    let now = UnixTime::now();
    let Some(tm) = now.local_tm() else {
        return now;
    };
    let since_midnight = tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec;
    now - Duration::from_secs(since_midnight.try_into().unwrap_or(0))
        + Duration::from_secs(12 * 3600)
}
