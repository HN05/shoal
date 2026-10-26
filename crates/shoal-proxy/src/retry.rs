//! Which upstream failures the proxy retries, and how long it waits.
use std::{sync::Mutex, time::Duration};

use hyper::{HeaderMap, StatusCode};
use tokio::time::Instant;

/// Why an upstream request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// A rate or usage limit refused the request.
    RateLimited,
    /// The upstream was overloaded.
    Overloaded,
    /// The upstream failed on its side.
    ServerError,
    /// The upstream could not be reached.
    Unreachable,
    /// Any other error, such as authentication or an invalid request, which a
    /// retry does not fix.
    Other,
}

impl Failure {
    /// The failure an upstream response status reports; `None` for a
    /// success or redirect.
    pub(crate) fn of_status(status: StatusCode) -> Option<Self> {
        Some(match status.as_u16() {
            100..=399 => return None,
            429 => Self::RateLimited,
            503 | 529 => Self::Overloaded,
            500 | 502 | 504 => Self::ServerError,
            _ => Self::Other,
        })
    }

    /// Whether the same request may succeed once the upstream recovers.
    pub fn retryable(self) -> bool {
        self != Self::Other
    }
}

/// The first wait after a failed attempt, doubling for each later one.
const FIRST_WAIT: Duration = Duration::from_secs(1);
/// The longest wait between attempts.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// How long the proxy retries through one outage before it passes failures
/// on to the agent, until a request succeeds again.
pub(crate) const OUTAGE_BUDGET: Duration = Duration::from_secs(5 * 60);

/// The upstream's current outage, which every request shares: once a
/// request gave up on it, requests fail at once instead of each waiting it
/// out again.
#[derive(Default)]
pub(crate) struct Outage(Mutex<State>);

#[derive(Default)]
enum State {
    /// The last request succeeded.
    #[default]
    Up,
    /// Requests have failed since this instant.
    Down(Instant),
    /// A request gave up on the outage.
    GivenUp,
}

impl Outage {
    /// The wait before trying again after attempt `attempt` failed, the
    /// upstream's own `Retry-After` when it gives one; `None`, giving up on
    /// the outage, when that would outlast its budget or a request already
    /// gave up.
    pub(crate) fn next_wait(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
    ) -> Option<Duration> {
        let now = Instant::now();
        let mut state = self.0.lock().unwrap();
        let started = match *state {
            State::Up => now,
            State::Down(started) => started,
            State::GivenUp => return None,
        };
        let wait = retry_after.unwrap_or_else(|| backoff(attempt));
        let fits = OUTAGE_BUDGET
            .checked_sub(now - started)
            .is_some_and(|remaining| wait <= remaining);
        *state = if fits {
            State::Down(started)
        } else {
            State::GivenUp
        };
        fits.then_some(wait)
    }

    /// A request succeeded, so the next failure starts a new outage.
    pub(crate) fn ended(&self) {
        *self.0.lock().unwrap() = State::Up;
    }
}

fn backoff(attempt: u32) -> Duration {
    FIRST_WAIT
        .saturating_mul(1 << attempt.saturating_sub(1).min(16))
        .min(MAX_WAIT)
}

/// The wait a `Retry-After` header in seconds asks for.
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_name_the_failure() {
        for (status, failure) in [
            (200, None),
            (302, None),
            (429, Some(Failure::RateLimited)),
            (503, Some(Failure::Overloaded)),
            (529, Some(Failure::Overloaded)),
            (500, Some(Failure::ServerError)),
            (504, Some(Failure::ServerError)),
            (401, Some(Failure::Other)),
            (400, Some(Failure::Other)),
        ] {
            let status = StatusCode::from_u16(status).unwrap();
            assert_eq!(Failure::of_status(status), failure, "{status}");
        }
    }

    #[test]
    fn waits_double_from_a_second_to_half_a_minute() {
        let waits: Vec<_> = (1..=7).map(|attempt| backoff(attempt).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(backoff(u32::MAX), MAX_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn an_outage_is_retried_for_its_budget_until_a_request_succeeds() {
        let outage = Outage::default();
        assert_eq!(outage.next_wait(1, None), Some(FIRST_WAIT));
        // The upstream's own wait wins, but not past the budget.
        let asked = Some(Duration::from_secs(90));
        assert_eq!(outage.next_wait(2, asked), asked);
        tokio::time::advance(OUTAGE_BUDGET - Duration::from_secs(60)).await;
        assert_eq!(outage.next_wait(3, None), Some(Duration::from_secs(4)));
        // A wait past the budget gives up, and later requests too.
        assert_eq!(outage.next_wait(4, asked), None);
        assert_eq!(outage.next_wait(1, None), None);
        outage.ended();
        assert_eq!(outage.next_wait(1, None), Some(FIRST_WAIT));
    }

    #[test]
    fn retry_after_reads_seconds() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert("retry-after", "30".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
    }
}
