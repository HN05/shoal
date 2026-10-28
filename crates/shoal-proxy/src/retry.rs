//! Which upstream failures the proxy retries, and how long it waits.
use std::{sync::Mutex, time::Duration};

use hyper::{HeaderMap, StatusCode};
use std::sync::Arc;

use crate::clock::Clock;

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
    /// The account's quota is spent, as OpenAI's `insufficient_quota` says
    /// even on a 429: waiting does not refill it, so no retry fixes it, but
    /// the agent's turn ends on it.
    Exhausted,
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
        !matches!(self, Self::Exhausted | Self::Other)
    }

    /// The failure an HTTP failure's body names, over the one its status
    /// does: a spent quota, which no wait refills, whatever the status says.
    pub(crate) fn of_error(self, error: Option<&serde_json::Value>) -> Self {
        let code = error.and_then(|error| error["code"].as_str().or(error["type"].as_str()));
        match code {
            Some(crate::stream::QUOTA_EXHAUSTED) => Self::Exhausted,
            _ => self,
        }
    }
}

/// How long the proxy waits between attempts and retries one outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    /// The first wait after a failed attempt, doubling for each later one.
    pub first_wait: Duration,
    /// The longest wait between attempts.
    pub max_wait: Duration,
    /// How long the proxy retries through one outage before it passes
    /// failures on to the agent, until a request succeeds again.
    pub budget: Duration,
}

/// The policy every proxy runs with; it has no options.
pub(crate) const POLICY: Policy = Policy {
    first_wait: Duration::from_secs(1),
    max_wait: Duration::from_secs(30),
    budget: Duration::from_secs(5 * 60),
};

impl Policy {
    fn backoff(&self, attempt: u32) -> Duration {
        self.first_wait
            .saturating_mul(1 << attempt.saturating_sub(1).min(16))
            .min(self.max_wait)
    }
}

/// How long an attempt may take, by the state of the upstream's outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bound {
    /// The last request succeeded, so the upstream may take its time.
    Unbounded,
    /// The upstream is down, with this much of the budget left.
    Within(Duration),
    /// A request gave up on the outage.
    GivenUp,
}

/// The upstream's current outage, which every request shares: once a
/// request gave up on it, requests fail at once instead of each waiting it
/// out again.
pub(crate) struct Outage {
    policy: Policy,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Default)]
enum State {
    /// The last request succeeded.
    #[default]
    Up,
    /// Requests have failed since this instant.
    Down(Duration),
    /// A request gave up on the outage.
    GivenUp,
}

impl Outage {
    pub(crate) fn new(policy: Policy, clock: Arc<dyn Clock>) -> Self {
        Self {
            policy,
            clock,
            state: Mutex::new(State::Up),
        }
    }

    /// The wait before trying again after attempt `attempt` failed, the
    /// upstream's own `Retry-After` when it gives one; `None`, giving up on
    /// the outage, when that would outlast its budget or a request already
    /// gave up.
    pub(crate) fn next_wait(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
    ) -> Option<Duration> {
        // Read under the lock, so concurrent failures see the outage start
        // in the order they record it.
        let mut state = self.state.lock().unwrap();
        let now = self.clock.now();
        let started = match *state {
            State::Up => now,
            State::Down(started) => started,
            State::GivenUp => return None,
        };
        let wait = retry_after.unwrap_or_else(|| self.policy.backoff(attempt));
        let fits = self
            .policy
            .budget
            .checked_sub(now.saturating_sub(started))
            .is_some_and(|remaining| wait <= remaining);
        *state = if fits {
            State::Down(started)
        } else {
            State::GivenUp
        };
        fits.then_some(wait)
    }

    /// How long an attempt may take now: as long as the upstream needs
    /// while it answers, the budget that is left while it is down.
    pub(crate) fn bound(&self) -> Bound {
        match *self.state.lock().unwrap() {
            State::Up => Bound::Unbounded,
            State::Down(started) => {
                let elapsed = self.clock.now().saturating_sub(started);
                Bound::Within(self.policy.budget.saturating_sub(elapsed))
            }
            State::GivenUp => Bound::GivenUp,
        }
    }

    /// Whether a request gave up on the outage, so retries stop too.
    pub(crate) fn given_up(&self) -> bool {
        matches!(*self.state.lock().unwrap(), State::GivenUp)
    }

    /// A request succeeded, so the next failure starts a new outage;
    /// whether one was under way.
    pub(crate) fn ended(&self) -> bool {
        let previous = std::mem::take(&mut *self.state.lock().unwrap());
        !matches!(previous, State::Up)
    }
}

/// The wait an upstream error's reset time asks for at Unix second `now`,
/// as a usage limit gives it: `resets_in_seconds`, or `resets_at` in Unix
/// seconds.
pub(crate) fn error_wait(error: &serde_json::Value, now: u64) -> Option<Duration> {
    if let Some(seconds) = error["resets_in_seconds"].as_u64() {
        return Some(Duration::from_secs(seconds));
    }
    Some(until(error["resets_at"].as_u64()?, now))
}

/// The wait at Unix second `now` until Anthropic's subscription usage limit
/// resets, which its `anthropic-ratelimit-unified-reset` header gives in
/// Unix seconds.
pub(crate) fn limit_reset(headers: &HeaderMap, now: u64) -> Option<Duration> {
    let reset = headers
        .get("anthropic-ratelimit-unified-reset")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(until(reset, now))
}

/// The wait from Unix second `now` until Unix second `at`.
fn until(at: u64, now: u64) -> Duration {
    Duration::from_secs(at.saturating_sub(now))
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
mod tests;
