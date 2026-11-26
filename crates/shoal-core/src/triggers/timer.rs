//! The `timer` source: firings at clock times on chosen days, or at an
//! interval from the last firing, in the local time zone or the one the
//! trigger names. Its schedule is a pure function of the times the daemon
//! records, so a restart neither resets nor repeats it.
use std::{fmt, str::FromStr};

use anyhow::{Context, Result, ensure};
use jiff::{
    Timestamp,
    civil::{Date, DateTime, Weekday},
    tz::{AmbiguousOffset, TimeZone},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{config::span::Span, time::UnixTime};

/// The longest a schedule of clock times can go without one: a week, with
/// a day to spare for a time a daylight-saving change moves.
const SEARCH_DAYS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub schedule: Schedule,
    /// The IANA name of its time zone; the local one when none.
    pub timezone: Option<String>,
    /// How old a missed firing may be and still run once the daemon is
    /// back: one interval for `every`, 24 hours for `at`, unless set.
    pub catch_up: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// The interval from the last firing.
    Every(Span),
    /// The clock times, earliest first, on the days it fires.
    At {
        times: Vec<ClockTime>,
        days: Vec<Day>,
    },
}

/// A time of day as config writes it, `HH:MM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockTime {
    hour: i8,
    minute: i8,
}

/// A day of the week as config writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

/// What the scheduled times since a timer last counted ask for at a moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// The scheduled time to fire for now, the newest missed one within
    /// `catch_up`.
    pub fire: Option<UnixTime>,
    /// The first and last of the scheduled times it skips, too old to
    /// catch up.
    pub skipped: Option<(UnixTime, UnixTime)>,
    /// The newest scheduled time it handled, from which it counts next.
    pub counted_to: UnixTime,
}

impl Timer {
    /// A timer firing every `every` or at the clock times `at` on `days`,
    /// every day without them, in `timezone`, catching up within `catch_up`.
    pub fn new(
        every: Option<Span>,
        at: Option<Vec<ClockTime>>,
        days: Option<Vec<Day>>,
        timezone: Option<String>,
        catch_up: Option<Span>,
    ) -> Result<Self> {
        let schedule = match (every, at) {
            (Some(every), None) => {
                ensure!(days.is_none(), "days applies to at, not every");
                Schedule::Every(every)
            }
            (None, Some(mut times)) => {
                ensure!(!times.is_empty(), "at names at least one time");
                times.sort();
                times.dedup();
                let mut days = days.unwrap_or_else(|| Day::ALL.to_vec());
                ensure!(
                    !days.is_empty(),
                    "days names at least one day; leave it out for every day"
                );
                days.sort();
                days.dedup();
                Schedule::At { times, days }
            }
            _ => anyhow::bail!("a timer sets one of every and at"),
        };
        if let Some(name) = &timezone {
            TimeZone::get(name).with_context(|| format!("unknown time zone {name}"))?;
        }
        let catch_up = catch_up.unwrap_or(match &schedule {
            Schedule::Every(every) => *every,
            Schedule::At { .. } => Span::from_secs(24 * 3600),
        });
        Ok(Self {
            schedule,
            timezone,
            catch_up,
        })
    }

    fn zone(&self) -> TimeZone {
        // The name was checked when the config was read.
        (self.timezone.as_deref())
            .and_then(|name| TimeZone::get(name).ok())
            .unwrap_or_else(TimeZone::system)
    }

    /// The first scheduled time after `counted_from`, for a timer that
    /// last fired at `last_fired`.
    pub fn next(&self, counted_from: UnixTime, last_fired: Option<UnixTime>) -> UnixTime {
        match &self.schedule {
            Schedule::Every(every) => {
                let every = every.duration().as_secs().max(1) as i64;
                let anchor = last_fired.unwrap_or(counted_from).seconds();
                let passed = (counted_from.seconds() - anchor).max(0);
                UnixTime::from_seconds(anchor + (passed / every + 1) * every)
            }
            Schedule::At { times, days } => self.clock_times(times, days, counted_from, true),
        }
    }

    /// The newest scheduled time at or before `now`, for a timer that last
    /// fired at `last_fired` and counts from `counted_from`.
    fn latest(
        &self,
        counted_from: UnixTime,
        last_fired: Option<UnixTime>,
        now: UnixTime,
    ) -> UnixTime {
        match &self.schedule {
            Schedule::Every(every) => {
                let every = every.duration().as_secs().max(1) as i64;
                let anchor = last_fired.unwrap_or(counted_from).seconds();
                let passed = (now.seconds() - anchor).max(0);
                UnixTime::from_seconds(anchor + passed / every * every)
            }
            Schedule::At { times, days } => self.clock_times(times, days, now, false),
        }
    }

    /// What the scheduled times after `counted_from` and up to `now` ask
    /// for: the newest fires when it is at most `catch_up` old, and every
    /// other one is skipped.
    pub fn due(
        &self,
        counted_from: UnixTime,
        last_fired: Option<UnixTime>,
        now: UnixTime,
    ) -> Option<Due> {
        let first = self.next(counted_from, last_fired);
        if first > now {
            return None;
        }
        let newest = self.latest(counted_from, last_fired, now).max(first);
        let fire = (now.duration_since(newest) <= self.catch_up.duration()).then_some(newest);
        let skipped_to = match fire {
            Some(_) if first == newest => None,
            Some(_) => Some(self.latest(
                counted_from,
                last_fired,
                newest - std::time::Duration::from_secs(1),
            )),
            None => Some(newest),
        };
        Some(Due {
            fire,
            skipped: skipped_to.map(|last| (first, last.max(first))),
            counted_to: newest,
        })
    }

    /// The first of `times` on `days` after `from`, or the last at or
    /// before it when not `after`.
    fn clock_times(
        &self,
        times: &[ClockTime],
        days: &[Day],
        from: UnixTime,
        after: bool,
    ) -> UnixTime {
        let zone = self.zone();
        let start = Timestamp::from_second(from.seconds()).unwrap_or(Timestamp::UNIX_EPOCH);
        let mut date = zone.to_datetime(start).date();
        // A day beyond, for a time a daylight-saving change moves across
        // midnight.
        date = step(date, after);
        for _ in 0..=SEARCH_DAYS + 1 {
            if days.contains(&Day::of(date.weekday())) {
                let instants = times.iter().map(|time| instant(&zone, date, *time));
                let found = match after {
                    true => instants.filter(|instant| *instant > from).min(),
                    false => instants.filter(|instant| *instant <= from).max(),
                };
                if let Some(found) = found {
                    return found;
                }
            }
            date = step(date, !after);
        }
        // Unreachable with a day to fire on; fire as late as it can.
        from
    }

    /// A firing's input document: the time it was scheduled for, the time
    /// it fired, the last firing before it and the trigger's name, as
    /// RFC 3339 times in the timer's zone.
    pub fn input(
        &self,
        trigger: &str,
        scheduled: UnixTime,
        fired: UnixTime,
        previous: Option<UnixTime>,
    ) -> Value {
        let zone = self.zone();
        let text = |time: UnixTime| {
            let timestamp = Timestamp::from_second(time.seconds()).unwrap_or(Timestamp::UNIX_EPOCH);
            timestamp
                .display_with_offset(zone.to_offset(timestamp))
                .to_string()
        };
        let mut input = json!({
            "trigger": trigger,
            "scheduled_at": text(scheduled),
            "fired_at": text(fired),
        });
        if let Some(previous) = previous {
            input["previous_at"] = Value::String(text(previous));
        }
        input
    }
}

/// The day after `date`, or before it when `back`.
fn step(date: Date, back: bool) -> Date {
    let stepped = if back {
        date.yesterday()
    } else {
        date.tomorrow()
    };
    stepped.unwrap_or(date)
}

/// When `time` on `date` happens in `zone`: the earlier of a time that
/// occurs twice, and the next valid minute for one a daylight-saving change
/// skips.
fn instant(zone: &TimeZone, date: Date, time: ClockTime) -> UnixTime {
    let mut datetime: DateTime = date.at(time.hour, time.minute, 0, 0);
    // A gap lasts at most a day, in minutes.
    for _ in 0..24 * 60 {
        let ambiguous = zone.to_ambiguous_timestamp(datetime);
        if !matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. })
            && let Ok(timestamp) = ambiguous.earlier()
        {
            return UnixTime::from_seconds(timestamp.as_second());
        }
        let Ok(later) = datetime.checked_add(jiff::Span::new().minutes(1)) else {
            break;
        };
        datetime = later;
    }
    UnixTime::from_seconds(zone.to_timestamp(datetime).map_or(0, Timestamp::as_second))
}

impl Day {
    pub const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    fn of(weekday: Weekday) -> Self {
        Self::ALL[weekday.to_monday_zero_offset() as usize]
    }
}

impl ClockTime {
    /// The first time after `from` that the local clock shows this time,
    /// as a daily timer fires at it.
    pub fn next_local(self, from: UnixTime) -> UnixTime {
        let daily = Timer {
            schedule: Schedule::At {
                times: vec![self],
                days: Day::ALL.to_vec(),
            },
            timezone: None,
            catch_up: Span::from_secs(0),
        };
        daily.next(from, None)
    }
}

impl FromStr for ClockTime {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let parsed = (text.split_once(':'))
            .filter(|(hour, minute)| hour.len() == 2 && minute.len() == 2)
            .and_then(|(hour, minute)| Some((hour.parse::<i8>().ok()?, minute.parse::<i8>().ok()?)))
            .filter(|(hour, minute)| (0..24).contains(hour) && (0..60).contains(minute));
        let (hour, minute) = parsed
            .with_context(|| format!("{text}: expected a time of day as HH:MM, such as 09:00"))?;
        Ok(Self { hour, minute })
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl Serialize for ClockTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
