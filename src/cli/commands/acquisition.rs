//! Allocation outcomes shared by every acquisition, and the queued wait for them.
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::time::sleep;

use super::{EXIT_BUSY, approvals};
use crate::{
    cli::{context::Context, output::Style},
    daemon::approvals::{ApprovalRequest, DecisionStatus},
    protocol::Body,
};

#[derive(Debug)]
pub(super) enum Acquisition<T> {
    Acquired(T),
    Busy(String),
    ApprovalPending(Box<ApprovalRequest>),
    ApprovalDenied(Box<ApprovalRequest>),
}

impl<T> Acquisition<T> {
    fn approval(request: Box<ApprovalRequest>) -> Self {
        if request.status == DecisionStatus::Denied {
            Self::ApprovalDenied(request)
        } else {
            Self::ApprovalPending(request)
        }
    }

    /// Domain handlers supply their successful payload and capacity details;
    /// approval rendering and exit status are shared by every acquisition.
    pub(super) fn finish(
        self,
        ctx: &Context,
        acquired: impl FnOnce(T) -> Result<()>,
        busy: impl FnOnce(&str) -> Value,
    ) -> Result<i32> {
        match self {
            Self::Acquired(value) => {
                acquired(value)?;
                Ok(0)
            }
            Self::ApprovalPending(request) | Self::ApprovalDenied(request) => {
                approvals::declined(ctx, &request)
            }
            Self::Busy(message) => {
                ctx.emit_styled(Style::Warning, &message, busy(&message))?;
                Ok(EXIT_BUSY)
            }
        }
    }
}

impl<T: TryFrom<Body, Error = anyhow::Error>> TryFrom<Body> for Acquisition<T> {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::ApprovalRequest(request) => Ok(Self::approval(request)),
            Body::Busy { message } => Ok(Self::Busy(message)),
            body => T::try_from(body).map(Self::Acquired),
        }
    }
}

/// Attempt once, or wait in the queue with elapsed-time feedback on a terminal.
pub(super) async fn obtain<T>(
    ctx: &Context,
    subject: &str,
    queue: bool,
    attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    if queue {
        let message = format!("Waiting for {subject}");
        ctx.progress(&message, retry(true, attempt)).await
    } else {
        retry(false, attempt).await
    }
}

/// Without a queue, return the first outcome. A queued request polls capacity
/// and pending approvals about once a second, keeping its place, until
/// acquisition, denial or an error.
async fn retry<T>(
    queue: bool,
    mut attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    loop {
        let outcome = attempt().await?;
        match outcome {
            Acquisition::Busy(_) | Acquisition::ApprovalPending(_) if queue => {
                sleep(Duration::from_secs(1)).await;
            }
            _ => return Ok(outcome),
        }
    }
}

#[cfg(test)]
mod tests;
