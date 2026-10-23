//! Rate limits as forges report them: `Retry-After`, and the remaining
//! requests and reset time of GitHub's `X-RateLimit-*` or GitLab's
//! `RateLimit-*` headers.
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transport::response::Response;

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
pub(crate) fn limit(response: &Response, now: u64) -> Option<Limit> {
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
    let refused = response.status == 429
        || (response.status == 403 && (retry_after.is_some() || remaining == Some(0)));
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
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: String::new(),
        }
    }

    #[test]
    fn refusals_wait_for_retry_after_then_the_reset_then_a_minute() {
        let now = 1_800_000_000;
        assert_eq!(
            limit(&response(429, &[("retry-after", "30")]), now),
            Some(Limit::Refused(now + 30))
        );
        let github = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1800000600"),
        ];
        assert_eq!(
            limit(&response(403, &github), now),
            Some(Limit::Refused(now + 600))
        );
        // GitLab's reset may count seconds from now.
        let gitlab = [("ratelimit-remaining", "0"), ("ratelimit-reset", "45")];
        assert_eq!(
            limit(&response(429, &gitlab), now),
            Some(Limit::Refused(now + 45))
        );
        assert_eq!(
            limit(&response(429, &[]), now),
            Some(Limit::Refused(now + DEFAULT_WAIT))
        );
        assert_eq!(
            limit(&response(429, &[("retry-after", "999999")]), now),
            Some(Limit::Refused(now + MAX_WAIT))
        );
    }

    #[test]
    fn permission_errors_and_remaining_requests_are_not_limits() {
        let now = 1_800_000_000;
        assert_eq!(limit(&response(403, &[]), now), None);
        let remaining = [
            ("x-ratelimit-remaining", "12"),
            ("x-ratelimit-reset", "1800000600"),
        ];
        assert_eq!(limit(&response(200, &remaining), now), None);
        let exhausted = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1800000600"),
        ];
        assert_eq!(
            limit(&response(200, &exhausted), now),
            Some(Limit::Exhausted(now + 600))
        );
    }

    #[test]
    fn limits_are_found_among_an_errors_causes() {
        let limited = RateLimited {
            host: "forge.example".into(),
            until: 1,
        };
        let error = anyhow::Error::new(limited.clone()).context("GET issues/1");
        assert_eq!(rate_limited(&error), Some(&limited));
        assert_eq!(rate_limited(&anyhow::anyhow!("HTTP 404")), None);
    }
}
