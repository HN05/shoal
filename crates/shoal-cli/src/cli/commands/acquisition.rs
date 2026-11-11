//! How the CLI shows an acquisition's outcome, and its queued wait.
use anyhow::Result;

use super::{EXIT_BUSY, approvals};
use crate::client::output::acquisition::NotAcquired;
use crate::{
    cli::{context::Context, output::Style},
    client::leases::{Acquisition, retry},
};

pub(super) trait Finish<T> {
    /// Domain handlers supply their successful payload and capacity details;
    /// approval rendering and exit status are shared by every acquisition.
    fn finish(
        self,
        ctx: &Context,
        acquired: impl FnOnce(T) -> Result<()>,
        busy: impl FnOnce(&str) -> NotAcquired,
    ) -> Result<i32>;
}

impl<T> Finish<T> for Acquisition<T> {
    fn finish(
        self,
        ctx: &Context,
        acquired: impl FnOnce(T) -> Result<()>,
        busy: impl FnOnce(&str) -> NotAcquired,
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
