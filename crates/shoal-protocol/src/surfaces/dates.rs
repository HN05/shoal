//! What a `DateTimeInput` takes: an ISO 8601 date, time or both, as it
//! enables them, within its bounds.
use anyhow::{Result, bail, ensure};
use serde_json::Value;

use super::Component;

/// The values a date input takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dates {
    pub form: DateForm,
    /// The earliest value it takes, as the input writes it.
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
    /// What `component`, a `DateTimeInput`, takes.
    pub(super) fn of(component: Component<'_>) -> Self {
        let enabled = |field| component.get(field).and_then(Value::as_bool) == Some(true);
        let form = match (enabled("enableDate"), enabled("enableTime")) {
            (true, true) => DateForm::DateTime,
            (true, false) => DateForm::Date,
            (false, true) => DateForm::Time,
            (false, false) => DateForm::Any,
        };
        Self {
            form,
            min: component.literal("min").map(str::to_owned),
            max: component.literal("max").map(str::to_owned),
        }
    }

    /// `text` as the value of the input `key`, refused unless it has the
    /// input's form and lies within its bounds.
    pub(super) fn accept(&self, key: &str, text: &str) -> Result<Value> {
        let form = form_of(text);
        ensure!(
            form.is_some_and(|form| self.form == DateForm::Any || form == self.form),
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

/// Whether `value` comes before `bound`, comparing the parts both write,
/// such as the date of a date and time against a date bound.
fn earlier(value: &str, bound: &str) -> bool {
    let shared = value.len().min(bound.len());
    value[..shared] < bound[..shared]
}

/// The form `text` has, none when it is no ISO 8601 date or time.
fn form_of(text: &str) -> Option<DateForm> {
    match text.split_once('T') {
        Some((date, time)) => (is_date(date) && is_time(time)).then_some(DateForm::DateTime),
        None if is_date(text) => Some(DateForm::Date),
        None if is_time(text) => Some(DateForm::Time),
        None => None,
    }
}

/// Whether `text` is a calendar date, `YYYY-MM-DD`.
fn is_date(text: &str) -> bool {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Some(year), Some(month), Some(day)) = (number(year, 4), number(month, 2), number(day, 2))
    else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// Whether `text` is a time of day, `HH:MM`, with seconds and their
/// fraction and a zone, `Z` or `±HH:MM`, where given.
fn is_time(text: &str) -> bool {
    let (clock, zone) = match text.find(['Z', '+', '-']) {
        Some(at) => text.split_at(at),
        None => (text, ""),
    };
    let zoned = match zone {
        "" | "Z" => true,
        zone => zone[1..]
            .split_once(':')
            .is_some_and(|(h, m)| hours_minutes(h, m)),
    };
    let mut parts = clock.splitn(3, ':');
    let (Some(hours), Some(minutes)) = (parts.next(), parts.next()) else {
        return false;
    };
    zoned && hours_minutes(hours, minutes) && parts.next().is_none_or(is_seconds)
}

/// Whether `hours` and `minutes` name a minute of the day.
fn hours_minutes(hours: &str, minutes: &str) -> bool {
    number(hours, 2).is_some_and(|hours| hours <= 23)
        && number(minutes, 2).is_some_and(|minutes| minutes <= 59)
}

/// Whether `text` is seconds, `SS`, with a fraction where given; 60 is the
/// leap second.
fn is_seconds(text: &str) -> bool {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    !fraction.is_empty()
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && number(whole, 2).is_some_and(|seconds| seconds <= 60)
}

/// `text` as a number of exactly `digits` decimal digits.
fn number(text: &str, digits: usize) -> Option<u32> {
    (text.len() == digits && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}
