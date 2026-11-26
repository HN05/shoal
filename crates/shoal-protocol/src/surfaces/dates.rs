//! What a `DateTimeInput` takes: an ISO 8601 date, time or both, as it
//! enables them, within its bounds.
use std::cmp::Ordering;

use anyhow::{Result, bail, ensure};
use serde_json::Value;

use super::{Component, Surface};

/// The values a date input takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dates {
    pub form: DateForm,
    /// The earliest value it takes, as the input writes it, from its data
    /// where it binds the bound.
    pub min: Option<String>,
    /// The latest value it takes.
    pub max: Option<String>,
}

/// Which parts of a moment a date input enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateForm {
    /// A date, such as `2026-11-26`.
    Date,
    /// A time, such as `14:30`, with seconds and a zone where given.
    Time,
    /// A date and time, such as `2026-11-26T14:30:00Z`.
    DateTime,
    /// Any of these, for an input that enables neither part.
    Any,
}

impl Dates {
    /// What `component`, a `DateTimeInput` of `surface` placed in `scope`,
    /// takes, its bounds read in that scope where they bind data.
    pub(super) fn of(surface: &Surface, component: Component<'_>, scope: &str) -> Self {
        let enabled = |field| component.get(field).and_then(Value::as_bool) == Some(true);
        let form = match (enabled("enableDate"), enabled("enableTime")) {
            (true, true) => DateForm::DateTime,
            (true, false) => DateForm::Date,
            (false, true) => DateForm::Time,
            (false, false) => DateForm::Any,
        };
        Self {
            form,
            min: (component.get("min")).and_then(|bound| surface.text(bound, scope)),
            max: (component.get("max")).and_then(|bound| surface.text(bound, scope)),
        }
    }

    /// `text` as the value of the input `key`, refused unless it is empty,
    /// unset, or has the input's form and lies within its bounds.
    pub(super) fn accept(&self, key: &str, text: &str) -> Result<Value> {
        // An unset date is empty, as the input starts.
        if text.is_empty() {
            return Ok(Value::String(String::new()));
        }
        let moment = Moment::parse(text);
        ensure!(
            moment.is_some_and(|moment| self.form == DateForm::Any || moment.form() == self.form),
            "{key} takes {}, not {text}",
            self.describe()
        );
        if let Some(min) = (self.min.as_deref()).filter(|min| earlier(text, min)) {
            bail!("{key} takes {min} or later, not {text}");
        }
        if let Some(max) = (self.max.as_deref()).filter(|max| earlier(max, text)) {
            bail!("{key} takes {max} or earlier, not {text}");
        }
        Ok(Value::String(text.to_owned()))
    }

    /// What it takes, in a few words.
    pub fn describe(&self) -> String {
        let form = match self.form {
            DateForm::Date => "a date such as 2026-11-26",
            DateForm::Time => "a time such as 14:30",
            DateForm::DateTime => "a date and time such as 2026-11-26T14:30:00Z",
            DateForm::Any => "an ISO 8601 date, time or both",
        };
        match (&self.min, &self.max) {
            (Some(min), Some(max)) => format!("{form}, from {min} to {max}"),
            (Some(min), None) => format!("{form}, from {min}"),
            (None, Some(max)) => format!("{form}, up to {max}"),
            (None, None) => form.to_owned(),
        }
    }
}

/// Whether `value` comes before `bound`, as moments: a date against the
/// date of a date and time in UTC, and a time only against a time. A bound
/// that is no moment, or another kind, bounds nothing.
fn earlier(value: &str, bound: &str) -> bool {
    let (Some(value), Some(bound)) = (Moment::parse(value), Moment::parse(bound)) else {
        return false;
    };
    value.compare(bound) == Some(Ordering::Less)
}

/// An ISO 8601 date, time or both, reduced to what orders it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Moment {
    /// Days since 1970-01-01.
    Date(i64),
    /// Seconds into the day, in UTC where the time names its zone.
    Time(f64),
    /// Seconds since 1970-01-01T00:00:00Z, taking a time without a zone as
    /// UTC.
    DateTime(f64),
}

impl Moment {
    /// The moment `text` writes, none when it is no ISO 8601 date or time.
    fn parse(text: &str) -> Option<Self> {
        match text.split_once('T') {
            Some((date, time)) => {
                let (days, seconds) = (days(date)?, seconds(time)?);
                Some(Self::DateTime(days as f64 * 86_400.0 + seconds))
            }
            None => (days(text).map(Self::Date)).or_else(|| seconds(text).map(Self::Time)),
        }
    }

    fn form(self) -> DateForm {
        match self {
            Self::Date(_) => DateForm::Date,
            Self::Time(_) => DateForm::Time,
            Self::DateTime(_) => DateForm::DateTime,
        }
    }

    /// How it orders against `other`, none for moments of no common kind.
    fn compare(self, other: Self) -> Option<Ordering> {
        let day = |seconds: f64| (seconds / 86_400.0).floor() as i64;
        match (self, other) {
            (Self::Date(a), Self::Date(b)) => Some(a.cmp(&b)),
            (Self::Date(a), Self::DateTime(b)) => Some(a.cmp(&day(b))),
            (Self::DateTime(a), Self::Date(b)) => Some(day(a).cmp(&b)),
            (Self::Time(a), Self::Time(b)) | (Self::DateTime(a), Self::DateTime(b)) => {
                a.partial_cmp(&b)
            }
            _ => None,
        }
    }
}

/// The days since 1970-01-01 of the calendar date `text`, `YYYY-MM-DD`.
fn days(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let (year, month, day) = (number(year, 4)?, number(month, 2)?, number(day, 2)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let length = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    (1..=length)
        .contains(&day)
        .then(|| civil_days(year, month, day))
}

/// The days since 1970-01-01 of a valid date, by Howard Hinnant's
/// `days_from_civil`.
fn civil_days(year: u32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let month = i64::from(month);
    let of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_era_days - 719_468
}

/// The seconds into the day of the time of day `text`, `HH:MM`, with
/// seconds and their fraction and a zone, `Z` or `±HH:MM`, where given; in
/// UTC where it names its zone.
fn seconds(text: &str) -> Option<f64> {
    let (clock, zone) = match text.find(['Z', '+', '-']) {
        Some(at) => text.split_at(at),
        None => (text, ""),
    };
    let offset = match zone {
        "" | "Z" => 0.0,
        zone => {
            let (hours, minutes) = zone[1..].split_once(':')?;
            let offset = minute_of_day(hours, minutes)? * 60.0;
            if zone.starts_with('-') {
                -offset
            } else {
                offset
            }
        }
    };
    let mut parts = clock.splitn(3, ':');
    let (hours, minutes) = (parts.next()?, parts.next()?);
    let seconds = match parts.next() {
        Some(seconds) => second(seconds)?,
        None => 0.0,
    };
    Some(minute_of_day(hours, minutes)? * 60.0 + seconds - offset)
}

/// The minute of the day `hours` and `minutes` name.
fn minute_of_day(hours: &str, minutes: &str) -> Option<f64> {
    let (hours, minutes) = (number(hours, 2)?, number(minutes, 2)?);
    (hours <= 23 && minutes <= 59).then(|| f64::from(hours * 60 + minutes))
}

/// The seconds `text` writes, `SS` with a fraction where given; 60 is the
/// leap second.
fn second(text: &str) -> Option<f64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let digits = !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit());
    let whole = number(whole, 2).filter(|seconds| *seconds <= 60)?;
    digits
        .then(|| format!("{whole}.{fraction}").parse().ok())
        .flatten()
}

/// `text` as a number of exactly `digits` decimal digits.
fn number(text: &str, digits: usize) -> Option<u32> {
    (text.len() == digits && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}
