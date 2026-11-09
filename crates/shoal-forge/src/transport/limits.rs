//! Rate limits as forges report them: `Retry-After`, and the remaining
//! requests and reset time of GitHub's `X-RateLimit-*` or GitLab's
//! `RateLimit-*` headers.
use std::time::Duration;

use shoal_time::UnixTime;

use crate::transport::response::Headed;

/// How long a forge that refuses requests without saying until when is left
/// alone.
const DEFAULT_WAIT: Duration = Duration::from_secs(60);
/// The longest wait a forge's headers can ask for; a longer one is a
/// malformed header rather than a limit.
const MAX_WAIT: Duration = Duration::from_secs(3600);
/// A reset header above this counts Unix seconds rather than seconds from now.
const UNIX_RESET: i64 = 1_000_000_000;

/// A forge refused, or would refuse, the account's requests until `until`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimited {
    pub host: String,
    pub until: UnixTime,
}

impl RateLimited {
    /// How long from `now` until the forge accepts requests again.
    pub fn wait_from(&self, now: UnixTime) -> Duration {
        self.until.duration_since(now)
    }
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} limits requests for another {} seconds",
            self.host,
            self.wait_from(UnixTime::now()).as_secs().max(1)
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
    Refused(UnixTime),
    /// The request was the last one before the limit resets at the time given.
    Exhausted(UnixTime),
}

impl Limit {
    pub fn until(&self) -> UnixTime {
        match self {
            Self::Refused(until) | Self::Exhausted(until) => *until,
        }
    }
}

/// The limit `response` reports at `now`, if any.
pub(crate) fn limit(response: &impl Headed, now: UnixTime) -> Option<Limit> {
    let header = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| response.header(name))
            .map(str::trim)
    };
    let seconds_from_now = |seconds: u64| now + Duration::from_secs(seconds);
    let retry_after = header(&["retry-after"])
        .and_then(|value| value.parse::<u64>().ok())
        .map(seconds_from_now);
    let remaining = header(&["x-ratelimit-remaining", "ratelimit-remaining"])
        .and_then(|value| value.parse::<u64>().ok());
    let reset = header(&["x-ratelimit-reset", "ratelimit-reset"])
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|reset| {
            if reset > UNIX_RESET {
                Some(UnixTime::from_seconds(reset))
            } else {
                u64::try_from(reset).ok().map(seconds_from_now)
            }
        });
    let bounded = |until: UnixTime| until.clamp(now + Duration::from_secs(1), now + MAX_WAIT);
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

#[cfg(test)]
mod tests;
