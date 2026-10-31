//! Rate limits as forges report them: `Retry-After`, and the remaining
//! requests and reset time of GitHub's `X-RateLimit-*` or GitLab's
//! `RateLimit-*` headers.
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transport::response::Headed;

/// How long a forge that refuses requests without saying until when is left
/// alone.
const DEFAULT_WAIT: u64 = 60;
/// The longest wait a forge's headers can ask for; a longer one is a
/// malformed header rather than a limit.
const MAX_WAIT: u64 = 3600;
/// A reset header above this counts Unix seconds rather than seconds from now.
const UNIX_RESET: u64 = 1_000_000_000;

/// A forge refused, or would refuse, the account's requests until `until`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimited {
    pub host: String,
    /// Unix seconds.
    pub until: u64,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} limits requests for another {} seconds",
            self.host,
            self.until.saturating_sub(unix_seconds()).max(1)
        )
    }
}

impl std::error::Error for RateLimited {}

/// The rate limit `error` or one of its causes reports.
pub fn rate_limited(error: &anyhow::Error) -> Option<&RateLimited> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RateLimited>())
}

/// What `response` says about the forge's limit at `now`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    /// The forge refused the request and accepts another at the time given.
    Refused(u64),
    /// The request was the last one before the limit resets at the time given.
    Exhausted(u64),
}

impl Limit {
    pub fn until(&self) -> u64 {
        match self {
            Self::Refused(until) | Self::Exhausted(until) => *until,
        }
    }
}

/// The limit `response` reports at `now`, if any.
pub(crate) fn limit(response: &impl Headed, now: u64) -> Option<Limit> {
    let number = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| response.header(name))
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let retry_after = number(&["retry-after"]).map(|seconds| now.saturating_add(seconds));
    let remaining = number(&["x-ratelimit-remaining", "ratelimit-remaining"]);
    let reset = number(&["x-ratelimit-reset", "ratelimit-reset"]).map(|reset| {
        if reset > UNIX_RESET {
            reset
        } else {
            now.saturating_add(reset)
        }
    });
    let bounded = |until: u64| until.clamp(now + 1, now + MAX_WAIT);
    // GitHub refuses with 403 as well; only a limit header tells it from a
    // permission error.
    let refused = response.status() == 429
        || (response.status() == 403 && (retry_after.is_some() || remaining == Some(0)));
    if refused {
        let until = retry_after.or(reset).unwrap_or(now + DEFAULT_WAIT);
        return Some(Limit::Refused(bounded(until)));
    }
    (remaining == Some(0))
        .then_some(reset)
        .flatten()
        .map(|reset| Limit::Exhausted(bounded(reset)))
}

/// Whole Unix seconds, or zero when the clock is before the epoch.
pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
