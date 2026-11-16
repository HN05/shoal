use jiff::{civil::date, tz::TimeZone};

use super::{ClockTime, Day, Due, Timer};
use crate::{config::span::Span, time::UnixTime};

const OSLO: &str = "Europe/Oslo";

/// The instant of `hour:minute` on the given day in Oslo, the earlier of
/// two.
fn oslo(year: i16, month: i8, day: i8, hour: i8, minute: i8) -> UnixTime {
    let zone = TimeZone::get(OSLO).unwrap();
    let timestamp = (zone.to_ambiguous_timestamp(date(year, month, day).at(hour, minute, 0, 0)))
        .earlier()
        .unwrap();
    UnixTime::from_seconds(timestamp.as_second())
}

fn at(times: &[&str], days: Option<Vec<Day>>, catch_up: Option<Span>) -> Timer {
    let times = times
        .iter()
        .map(|time| time.parse::<ClockTime>().unwrap())
        .collect();
    Timer::new(None, Some(times), days, Some(OSLO.to_owned()), catch_up).unwrap()
}

fn every(span: &str) -> Timer {
    Timer::new(
        Some(span.parse().unwrap()),
        None,
        None,
        Some(OSLO.to_owned()),
        None,
    )
    .unwrap()
}

fn weekdays() -> Option<Vec<Day>> {
    Some(vec![Day::Mon, Day::Tue, Day::Wed, Day::Thu, Day::Fri])
}

#[test]
fn clock_times_fire_on_their_days() {
    let timer = at(&["09:00", "14:00"], weekdays(), None);
    // Monday 2026-11-16, 10:00.
    let monday = oslo(2026, 11, 16, 10, 0);
    assert_eq!(timer.next(monday, None), oslo(2026, 11, 16, 14, 0));
    // Friday afternoon waits for Monday morning.
    let friday = oslo(2026, 11, 20, 15, 0);
    assert_eq!(timer.next(friday, None), oslo(2026, 11, 23, 9, 0));
    let daily = at(&["09:00"], None, None);
    assert_eq!(daily.next(friday, None), oslo(2026, 11, 21, 9, 0));
}

#[test]
fn every_counts_from_the_last_firing() {
    let timer = every("1h");
    let fired = oslo(2026, 11, 16, 10, 7);
    assert_eq!(timer.next(fired, Some(fired)), oslo(2026, 11, 16, 11, 7));
    // Counting from later, such as a resume, leaves out the times before it.
    let resumed = oslo(2026, 11, 16, 13, 30);
    assert_eq!(timer.next(resumed, Some(fired)), oslo(2026, 11, 16, 14, 7));
    // Never fired: one interval after it began counting.
    assert_eq!(timer.next(fired, None), oslo(2026, 11, 16, 11, 7));
}

#[test]
fn a_due_time_fires_once_and_older_missed_ones_are_skipped() {
    let timer = at(&["09:00"], None, None);
    let counted = oslo(2026, 11, 13, 9, 0);
    assert_eq!(
        timer.due(counted, Some(counted), oslo(2026, 11, 13, 20, 0)),
        None
    );
    // On time.
    let now = oslo(2026, 11, 14, 9, 0);
    assert_eq!(
        timer.due(counted, Some(counted), now),
        Some(Due {
            fire: Some(now),
            skipped: None,
            counted_to: now,
        })
    );
    // Down for three days: the newest runs within a day, the others skip.
    let now = oslo(2026, 11, 16, 12, 0);
    assert_eq!(
        timer.due(counted, Some(counted), now),
        Some(Due {
            fire: Some(oslo(2026, 11, 16, 9, 0)),
            skipped: Some((oslo(2026, 11, 14, 9, 0), oslo(2026, 11, 15, 9, 0))),
            counted_to: oslo(2026, 11, 16, 9, 0),
        })
    );
    // Back after the newest is older than catch_up: all skip.
    let short = at(&["09:00"], None, Some("1h".parse().unwrap()));
    assert_eq!(
        short.due(counted, Some(counted), now),
        Some(Due {
            fire: None,
            skipped: Some((oslo(2026, 11, 14, 9, 0), oslo(2026, 11, 16, 9, 0))),
            counted_to: oslo(2026, 11, 16, 9, 0),
        })
    );
}

#[test]
fn every_catches_up_within_one_interval() {
    let timer = every("1h");
    let fired = oslo(2026, 11, 16, 10, 0);
    // 11:00 and 12:00 were missed; 12:00 is within an hour of 12:30.
    let due = timer
        .due(fired, Some(fired), oslo(2026, 11, 16, 12, 30))
        .unwrap();
    assert_eq!(due.fire, Some(oslo(2026, 11, 16, 12, 0)));
    assert_eq!(
        due.skipped,
        Some((oslo(2026, 11, 16, 11, 0), oslo(2026, 11, 16, 11, 0)))
    );
}

#[test]
fn daylight_saving_changes_fire_each_time_once() {
    let timer = at(&["02:30"], None, None);
    // 02:00 to 03:00 does not exist on 2026-03-29 in Oslo: the next valid
    // minute is 03:00.
    let before = oslo(2026, 3, 28, 12, 0);
    assert_eq!(timer.next(before, None), oslo(2026, 3, 29, 3, 0));
    // 02:00 to 03:00 happens twice on 2026-10-25: it fires at the first.
    let before = oslo(2026, 10, 24, 12, 0);
    let first = timer.next(before, None);
    assert_eq!(first, oslo(2026, 10, 25, 2, 30));
    assert_eq!(timer.next(first, Some(first)), oslo(2026, 10, 26, 2, 30));
}

#[test]
fn input_names_the_times_in_the_timers_zone() {
    let timer = at(&["09:00"], None, None);
    let scheduled = oslo(2026, 11, 16, 9, 0);
    let input = timer.input(
        "triage",
        scheduled,
        scheduled + std::time::Duration::from_secs(5),
        None,
    );
    assert_eq!(input["trigger"], "triage");
    assert_eq!(input["scheduled_at"], "2026-11-16T09:00:00+01:00");
    assert_eq!(input["fired_at"], "2026-11-16T09:00:05+01:00");
    assert!(input.get("previous_at").is_none());
}

#[test]
fn refuses_what_no_timer_fires_on() {
    let time = || Some(vec!["09:00".parse().unwrap()]);
    let hour = || Some("1h".parse().unwrap());
    assert!(Timer::new(None, None, None, None, None).is_err());
    assert!(Timer::new(hour(), time(), None, None, None).is_err());
    assert!(Timer::new(hour(), None, weekdays(), None, None).is_err());
    assert!(Timer::new(None, time(), Some(Vec::new()), None, None).is_err());
    assert!(Timer::new(None, time(), None, Some("Mars/Olympus".into()), None).is_err());
    for text in ["9:00", "24:00", "09:60", "0900"] {
        assert!(text.parse::<ClockTime>().is_err(), "{text}");
    }
}
