//! The reset a failed turn's error text states: when the upstream that
//! refused its request accepts requests again, as a duration after a phrase
//! such as `reset after 14m40s`, or as a JSON error object's field.
use std::time::Duration;

use serde_json::Value;

use crate::time::UnixTime;

/// The phrases a duration until the reset follows, in lowercase.
const PHRASES: [&str; 4] = ["reset after", "resets in", "try again in", "retry after"];

/// The JSON fields that give the seconds until the reset.
const SECONDS_FIELDS: [&str; 2] = ["resets_in_seconds", "reset_seconds"];

/// The JSON field that gives the reset in Unix seconds.
const AT_FIELD: &str = "resets_at";

/// The reset `text` states, read at `now`; `None` when it states none.
pub(super) fn stated(text: &str, now: UnixTime) -> Option<UnixTime> {
    let lowercase = text.to_ascii_lowercase();
    after_phrase(&lowercase)
        .map(|wait| now + wait)
        .or_else(|| in_json(text, now))
}

/// The first duration that follows one of [`PHRASES`] in `text`.
fn after_phrase(text: &str) -> Option<Duration> {
    PHRASES.iter().find_map(|phrase| {
        text.match_indices(phrase)
            .find_map(|(at, _)| duration(text[at + phrase.len()..].trim_start()))
    })
}

/// The duration `text` starts with, in hours, minutes, seconds and
/// milliseconds such as `1h36m24s`, `14m40s` or `300s`; `None` when it
/// starts with none or a unit runs into a longer word, such as `5seconds`.
fn duration(text: &str) -> Option<Duration> {
    let mut rest = text;
    let mut total = Duration::ZERO;
    let mut components = 0;
    loop {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            break;
        }
        let amount: u64 = rest[..digits].parse().ok()?;
        let (unit, length) = unit(&rest[digits..])?;
        total += unit.checked_mul(u32::try_from(amount).ok()?)?;
        rest = &rest[digits + length..];
        components += 1;
    }
    let ends_word = !rest.starts_with(|c: char| c.is_ascii_alphabetic());
    (components > 0 && ends_word).then_some(total)
}

fn unit(text: &str) -> Option<(Duration, usize)> {
    [
        ("ms", Duration::from_millis(1)),
        ("h", Duration::from_secs(60 * 60)),
        ("m", Duration::from_secs(60)),
        ("s", Duration::from_secs(1)),
    ]
    .into_iter()
    .find(|(name, _)| text.starts_with(name))
    .map(|(name, unit)| (unit, name.len()))
}

/// The reset the first JSON object in `text` that states one gives, in
/// any of its nested objects.
fn in_json(text: &str, now: UnixTime) -> Option<UnixTime> {
    text.match_indices('{').find_map(|(at, _)| {
        let object = serde_json::Deserializer::from_str(&text[at..])
            .into_iter::<Value>()
            .next()?
            .ok()?;
        field_reset(&object, now)
    })
}

/// The reset one of `value`'s fields, or a nested object's, gives.
fn field_reset(value: &Value, now: UnixTime) -> Option<UnixTime> {
    let Value::Object(fields) = value else {
        return None;
    };
    let seconds = |field: &str| fields.get(field).and_then(Value::as_f64);
    SECONDS_FIELDS
        .iter()
        .find_map(|field| seconds(field))
        .filter(|seconds| *seconds >= 0.0)
        .map(|seconds| now + Duration::from_secs_f64(seconds.min(u32::MAX.into())))
        .or_else(|| seconds(AT_FIELD).map(|at| UnixTime::from_seconds(at as i64)))
        .or_else(|| fields.values().find_map(|nested| field_reset(nested, now)))
}

#[cfg(test)]
mod tests;
