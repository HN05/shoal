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

    /// How much of the budget is left while the upstream is down, which
    /// bounds each attempt then; `None` otherwise, when an attempt may take
    /// as long as the upstream needs.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        match *self.state.lock().unwrap() {
            State::Down(started) => {
                let elapsed = self.clock.now().saturating_sub(started);
                Some(self.policy.budget.saturating_sub(elapsed))
            }
            State::Up | State::GivenUp => None,
        }
    }

    /// A request succeeded, so the next failure starts a new outage.
    pub(crate) fn ended(&self) {
        *self.state.lock().unwrap() = State::Up;
    }
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
