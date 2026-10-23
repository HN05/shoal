//! Sending a request again after a failure that may pass, such as a dropped
//! connection or a busy forge, where the request says that is safe.
use std::{future::Future, time::Duration};

use anyhow::Result;
use serde_json::Value;

/// How many times a request is sent before its failure is returned.
const ATTEMPTS: u32 = 4;
/// The wait before the second attempt, doubled before each later one.
const FIRST_WAIT: Duration = Duration::from_secs(1);

/// Which failures a request may be sent again after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resend {
    Never,
    /// Only when the forge did not get it or asked to try later, such as an
    /// upload that would otherwise be kept twice.
    Unreceived,
    /// Also after a connection broke while the forge may have acted on it,
    /// since doing it twice changes nothing.
    Always,
}

/// A failure that may pass when the request is sent again.
#[derive(Debug)]
pub(crate) enum Transient {
    /// The connection failed before the request was sent.
    Unsent(String),
    /// The connection broke or timed out, or a server or proxy failed, after
    /// the request may have arrived.
    Interrupted(String),
    /// The forge answered that it did not act on the request for now.
    Busy(String),
}

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Unsent(message) | Self::Interrupted(message) | Self::Busy(message)) = self;
        f.write_str(message)
    }
}

impl std::error::Error for Transient {}

impl Transient {
    /// The failure curl's exit `code` reports, if it may pass.
    pub(crate) fn from_curl(code: Option<i32>, message: String) -> Option<Self> {
        match code? {
            // Resolving the host, connecting, or the TLS handshake.
            6 | 7 | 35 => Some(Self::Unsent(message)),
            // A timeout, a failed send or receive, or no reply.
            28 | 52 | 55 | 56 => Some(Self::Interrupted(message)),
            _ => None,
        }
    }

    /// The failure an HTTP `status` reports, if it may pass. Only 429 and 503
    /// say the request was not acted on; a proxy may answer another 5xx after
    /// the forge did.
    pub(crate) fn from_status(status: u16, message: String) -> Option<Self> {
        match status {
            429 | 503 => Some(Self::Busy(message)),
            500..600 => Some(Self::Interrupted(message)),
            _ => None,
        }
    }
}

impl Resend {
    fn allows(self, error: &anyhow::Error) -> bool {
        match error.downcast_ref::<Transient>() {
            Some(Transient::Unsent(_) | Transient::Busy(_)) => self != Self::Never,
            Some(Transient::Interrupted(_)) => self == Self::Always,
            None => false,
        }
    }
}

/// What `send` returns, calling it again, after a longer wait each time,
/// while it fails in a way `resend` allows and attempts remain.
pub(crate) async fn resending<F, Fut>(resend: Resend, mut send: F) -> Result<Value>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let mut wait = FIRST_WAIT;
    for _ in 1..ATTEMPTS {
        match send().await {
            Err(error) if resend.allows(&error) => {
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            result => return result,
        }
    }
    send().await
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use anyhow::anyhow;
    use tokio::time::Instant;

    use super::*;

    /// Send with `resend` through scripted results, the last repeating,
    /// returning the result, the attempts made and the time waited.
    async fn script(
        resend: Resend,
        results: Vec<fn() -> Result<Value>>,
    ) -> (Result<Value>, usize, Duration) {
        let attempts = Cell::new(0);
        let results = RefCell::new(results);
        let start = Instant::now();
        let result = resending(resend, || {
            attempts.set(attempts.get() + 1);
            let results = results.borrow();
            let next = results[(attempts.get() - 1).min(results.len() - 1)];
            async move { next() }
        })
        .await;
        (result, attempts.get(), start.elapsed())
    }

    fn unsent() -> Result<Value> {
        Err(anyhow!(Transient::Unsent("could not connect".into())).context("POST x"))
    }

    fn interrupted() -> Result<Value> {
        Err(Transient::Interrupted("timed out".into()).into())
    }

    fn busy() -> Result<Value> {
        Err(Transient::Busy("HTTP 503: busy".into()).into())
    }

    fn refused() -> Result<Value> {
        Err(anyhow!("HTTP 422: invalid"))
    }

    fn sent() -> Result<Value> {
        Ok(Value::Bool(true))
    }

    #[tokio::test(start_paused = true)]
    async fn passing_failures_are_sent_again_after_longer_waits() {
        let (result, attempts, waited) = script(Resend::Unreceived, vec![unsent, busy, sent]).await;
        assert_eq!(result.unwrap(), Value::Bool(true));
        assert_eq!((attempts, waited), (3, Duration::from_secs(3)));

        let (result, attempts, waited) = script(Resend::Always, vec![interrupted, sent]).await;
        assert!(result.is_ok());
        assert_eq!((attempts, waited), (2, Duration::from_secs(1)));
    }

    #[tokio::test(start_paused = true)]
    async fn other_failures_and_requests_are_sent_once() {
        for (resend, failure) in [
            (Resend::Never, unsent as fn() -> Result<Value>),
            (Resend::Never, busy),
            (Resend::Unreceived, interrupted),
            (Resend::Always, refused),
        ] {
            let (result, attempts, waited) = script(resend, vec![failure, sent]).await;
            assert!(result.is_err(), "{resend:?}");
            assert_eq!((attempts, waited), (1, Duration::ZERO), "{resend:?}");
        }
    }

    #[test]
    fn only_statuses_that_refuse_for_now_are_busy() {
        let kind = |status| match Transient::from_status(status, String::new()) {
            Some(Transient::Busy(_)) => "busy",
            Some(Transient::Interrupted(_)) => "interrupted",
            Some(Transient::Unsent(_)) => "unsent",
            None => "permanent",
        };
        assert_eq!(kind(429), "busy");
        assert_eq!(kind(503), "busy");
        assert_eq!(kind(502), "interrupted");
        assert_eq!(kind(504), "interrupted");
        assert_eq!(kind(422), "permanent");
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_failure_is_returned_once_attempts_run_out() {
        let (result, attempts, waited) = script(Resend::Always, vec![busy]).await;
        assert_eq!(format!("{:#}", result.unwrap_err()), "HTTP 503: busy");
        assert_eq!((attempts, waited), (4, Duration::from_secs(7)));
    }
}
