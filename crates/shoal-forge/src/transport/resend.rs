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
mod tests;
