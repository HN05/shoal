//! Shared syntax rules for user-supplied names and free-text reasons.
use anyhow::{Result, ensure};

pub const MAX_NAME_LEN: usize = 64;
pub const MAX_REASON_LEN: usize = 256;

/// Portable identifier: ASCII letters, digits, `-` or `_`, starting with a
/// letter or digit. Used for workspaces, repositories, and simulator profiles.
pub fn name(kind: &str, value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_NAME_LEN
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "{kind} names must be 1–{MAX_NAME_LEN} ASCII letters, digits, hyphens or underscores, starting with a letter or digit"
    );
    Ok(())
}

/// Lowercase identifier used for ports and resources, which also map onto
/// environment variables and configuration keys.
pub fn lowercase_name(kind: &str, value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_NAME_LEN
            && value.as_bytes()[0].is_ascii_lowercase()
            && value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-'),
        "{kind} names must start with a lowercase letter and contain only lowercase letters, digits, _ or - (max {MAX_NAME_LEN})"
    );
    Ok(())
}

/// Optional single-line explanation attached to a lease or reservation.
pub fn reason(kind: &str, value: Option<&str>) -> Result<()> {
    ensure!(
        value.is_none_or(|text| !text.trim().is_empty()
            && text.len() <= MAX_REASON_LEN
            && !text.contains(['\n', '\r'])),
        "{kind} reason must be a nonempty single line (max {MAX_REASON_LEN} bytes)"
    );
    Ok(())
}
