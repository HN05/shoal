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
