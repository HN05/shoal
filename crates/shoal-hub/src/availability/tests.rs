use std::time::Duration;

use shoal_protocol::hub::availability::{
    Availability, AvailabilityReason, AvailabilitySetting, Classified,
};

use super::{
    classify,
    presence::{History, Span},
};
use crate::time::UnixTime;

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;

fn now() -> UnixTime {
    UnixTime::from_seconds(100 * DAY as i64)
}

fn ago(seconds: u64) -> UnixTime {
    now() - Duration::from_secs(seconds)
}

/// A machine first seen `since` seconds ago, there through `spans`, each
/// from and to so many seconds ago.
fn history(since: u64, spans: &[(u64, u64)]) -> History {
    History {
        since: Some(ago(since)),
        spans: (spans.iter())
            .map(|&(start, end)| Span {
                start: ago(start),
                end: ago(end),
            })
            .collect(),
    }
}

fn auto(battery: Option<bool>, history: &History) -> Classified {
    classify(battery, AvailabilitySetting::Auto, history, now())
}

fn connected(availability: Availability, percent: u8) -> Classified {
    Classified {
        availability,
        reason: AvailabilityReason::Connected { percent },
    }
}

#[test]
fn a_new_machine_goes_by_its_battery() {
    let day_old = history(DAY - 60, &[(DAY - 60, 0)]);
    let new = |battery| auto(battery, &day_old);
    assert_eq!(new(Some(true)).availability, Availability::Intermittent);
    assert_eq!(new(Some(false)).availability, Availability::AlwaysOn);
    assert_eq!(new(None).availability, Availability::Intermittent);
    assert_eq!(
        new(Some(false)).reason,
        AvailabilityReason::New {
            battery: Some(false)
        }
    );
    assert_eq!(
        auto(Some(false), &History::default()).reason,
        AvailabilityReason::New {
            battery: Some(false)
        }
    );
}

#[test]
fn after_a_day_history_alone_decides() {
    // Connected throughout: a plugged-in laptop is always on.
    let throughout = history(2 * DAY, &[(2 * DAY, 30)]);
    assert_eq!(
        auto(Some(true), &throughout),
        connected(Availability::AlwaysOn, 100)
    );
    // Down 10% of the two days it has been known.
    let down = history(2 * DAY, &[(2 * DAY, 6 * HOUR), (HOUR + 12 * 60, 0)]);
    assert_eq!(
        auto(Some(false), &down),
        connected(Availability::Intermittent, 90)
    );
}

#[test]
fn the_share_counts_only_the_last_week() {
    // Known for a month, down for the week's first half day only.
    let spans = [(30 * DAY, 8 * DAY), (7 * DAY - 12 * HOUR, 0)];
    let classified = auto(Some(true), &history(30 * DAY, &spans));
    let AvailabilityReason::Connected { percent } = classified.reason else {
        panic!("{classified:?}");
    };
    assert_eq!(percent, 92);
}

#[test]
fn a_current_absence_counts_once_it_reaches_five_minutes() {
    let short = history(2 * DAY, &[(2 * DAY, 4 * 60)]);
    assert_eq!(auto(None, &short), connected(Availability::AlwaysOn, 100));
    let long = history(2 * DAY, &[(2 * DAY, 3 * HOUR)]);
    assert_eq!(auto(None, &long), connected(Availability::Intermittent, 93));
}

#[test]
fn the_setting_overrides_the_guess() {
    let throughout = history(2 * DAY, &[(2 * DAY, 0)]);
    let classified = classify(
        Some(false),
        AvailabilitySetting::Intermittent,
        &throughout,
        now(),
    );
    assert_eq!(classified.availability, Availability::Intermittent);
    assert_eq!(classified.reason, AvailabilityReason::Set);
}
