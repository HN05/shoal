//! Why a reviewer failed, from the phrases its backend reports
//! unavailability with. Only known phrases count, so a review that discusses
//! rate limits is never taken for one that hit them.
use serde_json::Value;

use super::FailureReason;

/// The most of a text or diagnostic stream that is read.
pub(super) const SCAN_LIMIT: usize = 64 * 1024;

/// The failure a backend's message names, if it names a known one.
pub fn classify_text(text: &str) -> Option<FailureReason> {
    let text = head(text, SCAN_LIMIT).to_lowercase().replace('’', "'");
    if usage_limit(&text) {
        Some(FailureReason::UsageLimit)
    } else if quota_exhausted(&text) {
        Some(FailureReason::QuotaExhausted)
    } else if rate_limit(&text) {
        Some(FailureReason::RateLimit)
    } else if authentication_failed(&text) {
        Some(FailureReason::AuthenticationFailed)
    } else {
        None
    }
}

/// The failure in a result's error fields, or in a Claude result's text
/// when it carries no structured review; never the review's own text.
pub fn classify_envelope(raw: &Value) -> Option<FailureReason> {
    let raw = raw.as_object()?;
    let mut fields: Vec<&Value> = ["error", "errors"]
        .iter()
        .filter_map(|key| raw.get(*key))
        .collect();
    if let Some(result @ Value::String(text)) = raw.get("result") {
        let unstructured = raw.get("structured_output").is_none_or(Value::is_null)
            || raw.get("is_error").is_some_and(truthy)
            || raw.get("api_error_status").is_some_and(truthy);
        // A review serialized in the result may discuss quotas or rate limits.
        if unstructured && serde_json::from_str::<Value>(text).is_err() {
            fields.push(result);
        }
    }
    let named = fields.into_iter().find_map(|field| match field {
        Value::String(text) => classify_text(text),
        Value::Object(_) | Value::Array(_) => classify_text(&field.to_string()),
        _ => None,
    });
    named.or_else(|| {
        (raw.get("api_error_status").and_then(Value::as_i64) == Some(429))
            .then_some(FailureReason::RateLimit)
    })
}

/// The failure a CLI's diagnostic output names in its error lines and error
/// events, ignoring echoed prompts and tool output.
pub fn classify_diagnostic(text: &str) -> Option<FailureReason> {
    text.lines().find_map(|line| {
        let line = strip_colors(line);
        let line = line.trim();
        match serde_json::from_str::<Value>(line) {
            Ok(event)
                if matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("error" | "turn.failed")
                ) =>
            {
                classify_envelope(&event).or_else(|| {
                    event
                        .get("message")
                        .and_then(Value::as_str)
                        .and_then(classify_text)
                })
            }
            _ if reports_error(line) => classify_text(line),
            _ => None,
        }
    })
}

fn usage_limit(text: &str) -> bool {
    contains_any(
        text,
        product(&[
            &["you've ", "you have "],
            &["hit ", "reached "],
            &["your "],
            &["", "chatgpt ", "claude "],
            &["", "usage "],
            &["limit"],
        ]),
    ) || contains_any(
        text,
        product(&[
            &["usage", "subscription", "weekly", "session"],
            &[" limit ", " limits "],
            &["", "has been ", "is "],
            &["reached", "exceeded"],
        ]),
    ) || contains_any(
        text,
        [
            "usage_limit_reached",
            "out of usage",
            "out of extra usage",
            "no usage remaining",
        ]
        .map(String::from),
    )
}

fn quota_exhausted(text: &str) -> bool {
    contains_any(
        text,
        product(&[&["insufficient", "exhausted"], &["_", " "], &["quota"]]),
    ) || contains_any(
        text,
        product(&[
            &["quota"],
            &["_", " "],
            &["", "has been ", "is "],
            &["exceeded", "exhausted"],
        ]),
    ) || contains_any(
        text,
        product(&[
            &["exceeded "],
            &["", "your ", "the "],
            &["", "current "],
            &["quota"],
        ]),
    ) || contains_any(
        text,
        [
            "billing_hard_limit_reached",
            "credit balance is too low",
            "insufficient credits",
            "out of credits",
        ]
        .map(String::from),
    )
}

fn rate_limit(text: &str) -> bool {
    contains_any(
        text,
        [
            "rate_limit",
            "rate limit",
            "rate-limit",
            "too many requests",
        ]
        .map(String::from),
    ) || text
        .match_indices("429")
        .any(|(index, _)| status_429(text, index))
}

fn authentication_failed(text: &str) -> bool {
    contains_any(
        text,
        [
            "authentication_error",
            "invalid_api_key",
            "sign in with chatgpt",
        ]
        .map(String::from),
    ) || text.lines().any(|line| {
        line.find("sign in with ")
            .is_some_and(|start| line[start..].contains("claude auth login"))
    })
}

/// Whether the `429` at `index` of `text` is an HTTP status: a whole word
/// after `(`, or after `http`, `http error`, `status` or `status code` and
/// separators.
fn status_429(text: &str, index: usize) -> bool {
    let after = text[index + 3..].chars().next();
    if after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
        return false;
    }
    let before = &text[..index];
    if before.ends_with('(') {
        return true;
    }
    let after_separators = |separators: &[char]| {
        let trimmed = before.trim_end_matches(separators);
        (trimmed.len() < before.len()).then_some(trimmed)
    };
    after_separators(&[':', ' '])
        .is_some_and(|rest| rest.ends_with("http") || rest.ends_with("http error"))
        || after_separators(&[':', ' ', '='])
            .is_some_and(|rest| rest.ends_with("status") || rest.ends_with("status code"))
}

/// Whether a diagnostic line, after an optional timestamp, opens the way
/// CLIs report errors and unavailability.
fn reports_error(line: &str) -> bool {
    let line = line.to_lowercase();
    let line = without_timestamp(&line);
    let word = |prefix: &str| {
        line.strip_prefix(prefix)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    };
    word("error")
        || word("api error")
        || word("quota")
        || [
            "::error::",
            "you've hit your ",
            "you have hit your ",
            "you've reached your ",
            "you have reached your ",
            "usage limit",
            "usage_limit",
            "insufficient_quota",
            "exhausted_quota",
            "credit balance is too low",
            "rate limit",
            "rate_limit",
            "rate-limit",
            "too many requests",
            "sign in with ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        || (line.starts_with("http")
            && line
                .find("429")
                .is_some_and(|index| status_429(line, index)))
}

/// `line` without a leading `YYYY-…` timestamp and the space after it.
fn without_timestamp(line: &str) -> &str {
    let bytes = line.as_bytes();
    let stamped = bytes.len() > 5 && bytes[..4].iter().all(u8::is_ascii_digit) && bytes[4] == b'-';
    if !stamped {
        return line;
    }
    match line.find(char::is_whitespace) {
        Some(end) => line[end..].trim_start(),
        None => line,
    }
}

/// `line` without ANSI color sequences.
fn strip_colors(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("\x1b[") {
        plain.push_str(&rest[..start]);
        let sequence = &rest[start + 2..];
        let end = sequence
            .find(|c: char| !c.is_ascii_digit() && c != ';')
            .unwrap_or(sequence.len());
        match sequence[end..].strip_prefix('m') {
            Some(after) => rest = after,
            None => {
                plain.push_str("\x1b[");
                rest = sequence;
            }
        }
    }
    plain.push_str(rest);
    plain
}

/// Python's truthiness, which the bot's classification follows.
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// The first `limit` bytes of `text`, cut at a character boundary.
pub(super) fn head(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The last `limit` bytes of `bytes`, as text.
pub(super) fn tail(bytes: &[u8], limit: usize) -> String {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(limit)..]).into_owned()
}

/// Every phrase made of one choice from each part, in order.
fn product(parts: &[&[&str]]) -> Vec<String> {
    parts.iter().fold(vec![String::new()], |phrases, choices| {
        phrases
            .iter()
            .flat_map(|phrase| {
                choices
                    .iter()
                    .map(move |choice| format!("{phrase}{choice}"))
            })
            .collect()
    })
}

fn contains_any(text: &str, phrases: impl IntoIterator<Item = String>) -> bool {
    phrases.into_iter().any(|phrase| text.contains(&phrase))
}

#[cfg(test)]
mod tests;
