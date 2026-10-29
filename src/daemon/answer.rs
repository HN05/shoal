//! How the daemon answers a request. A request's handler chooses the shape
//! of its answer; sending it is the same for every request.
use anyhow::Result;
use futures_util::{FutureExt, future::BoxFuture};
use tokio::{io::AsyncWrite, net::UnixStream, sync::OwnedRwLockReadGuard};

use super::workspace::{self, Manager};
use crate::protocol::{self, Body, ErrorCode, Response};

#[cfg(test)]
mod tests;

/// The guard a request holds while it runs, which keeps a daemon update from
/// restarting the daemon under it.
pub(super) type OperationGuard = OwnedRwLockReadGuard<()>;

/// What a request that keeps its connection does with it, given the stream,
/// the request's ID and its operation guard.
pub(super) type Connection =
    Box<dyn FnOnce(UnixStream, u64, OperationGuard) -> BoxFuture<'static, Result<()>> + Send>;

/// How the daemon answers a request.
#[allow(
    clippy::large_enum_variant,
    reason = "each request has one answer, moved straight into its reply"
)]
pub(super) enum Answer {
    /// One reply.
    Body(Body),
    /// A listing split into frames that fit, sent before `ok`.
    Pages(Result<Vec<Body>>),
    /// A wait that releases the operation guard, so updates need not wait for it.
    Wait(BoxFuture<'static, Result<Body>>),
    /// A request that keeps the connection, such as an execution or a watch.
    Connection(Connection),
}

impl Answer {
    /// A listing sent in as many `page` frames as it needs, so a long one
    /// never exceeds one frame.
    pub(super) fn pages<T: serde::Serialize>(
        listed: Result<Vec<T>>,
        page: fn(Vec<T>) -> Body,
    ) -> Self {
        match listed {
            Ok(items) => Self::Pages(
                protocol::frame_pages(items).map(|pages| pages.into_iter().map(page).collect()),
            ),
            Err(error) => Self::Body(Body::error(
                ErrorCode::OperationFailed,
                format!("{error:#}"),
            )),
        }
    }

    pub(super) fn wait(wait: impl Future<Output = Result<Body>> + Send + 'static) -> Self {
        Self::Wait(wait.boxed())
    }

    pub(super) fn connection<F, Fut>(keep: F) -> Self
    where
        F: FnOnce(UnixStream, u64, OperationGuard) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self::Connection(Box::new(move |stream, request_id, guard| {
            keep(stream, request_id, guard).boxed()
        }))
    }

    /// The reply to an operation that failed.
    pub(super) fn failed(error: &anyhow::Error) -> Self {
        Self::Body(Body::error(error_code(error), format!("{error:#}")))
    }

    /// Send the answer to `request_id`. The guard is held until the reply is
    /// written, except by a wait.
    pub(super) async fn send(
        self,
        mut stream: UnixStream,
        request_id: u64,
        manager: &Manager,
        guard: OperationGuard,
    ) -> Result<()> {
        match self {
            Self::Body(body) => reply(&mut stream, request_id, body).await,
            Self::Pages(pages) => write_pages(&mut stream, request_id, pages?).await,
            // A cancelled wait leaves its updates pending, so an update need
            // not wait for it; clients wait again on the replacement.
            Self::Wait(wait) => {
                drop(guard);
                answer_wait(stream, request_id, manager, wait).await
            }
            Self::Connection(keep) => keep(stream, request_id, guard).await,
        }
    }
}

/// The code of an operation's failure: specific where the caller can act on
/// it, otherwise `operation_failed`.
fn error_code(error: &anyhow::Error) -> ErrorCode {
    if error.downcast_ref::<workspace::UnprovenStop>().is_some() {
        ErrorCode::UnprovenStop
    } else if error.downcast_ref::<workspace::RestartWaits>().is_some() {
        ErrorCode::RestartWaits
    } else {
        ErrorCode::OperationFailed
    }
}

/// Write the reply to `request_id`, or an error saying it does not fit in a
/// frame; the oversized reply still fails the connection, for the log.
async fn reply(stream: &mut (impl AsyncWrite + Unpin), request_id: u64, body: Body) -> Result<()> {
    let Err(error) = protocol::write(stream, &Response::new(request_id, body)).await else {
        return Ok(());
    };
    if let Some(too_large) = error.downcast_ref::<protocol::FrameTooLarge>() {
        let message = format!("the daemon cannot send its response: {too_large}");
        let body = Body::error(ErrorCode::OperationFailed, message);
        protocol::write(stream, &Response::new(request_id, body)).await?;
    }
    Err(error)
}

/// Send each page, then `ok`.
async fn write_pages(stream: &mut UnixStream, request_id: u64, pages: Vec<Body>) -> Result<()> {
    for page in pages {
        protocol::write(stream, &Response::new(request_id, page)).await?;
    }
    protocol::write(stream, &Response::new(request_id, Body::Ok)).await
}

/// Answer a request that waits on the daemon without holding off its restart.
/// A closed connection cancels the wait. A restarting daemon could cut the
/// reply off, so it closes the connection unanswered instead: the client asks
/// again on the replacement, where the awaited result remains pending.
pub(super) async fn answer_wait(
    mut stream: UnixStream,
    request_id: u64,
    manager: &Manager,
    wait: impl Future<Output = Result<Body>>,
) -> Result<()> {
    let result = tokio::select! {
        result = wait => result,
        () = hangup(&mut stream) => return Ok(()),
    };
    let body = result
        .unwrap_or_else(|error| Body::error(ErrorCode::OperationFailed, format!("{error:#}")));
    // Hold the operation guard through the write so a restart cannot begin
    // while the reply is in flight.
    let Some(_operation) = manager.gates.try_operation() else {
        return Ok(());
    };
    protocol::write(&mut stream, &Response::new(request_id, body)).await
}

/// Wait until the client hangs up. A client sends nothing after its request,
/// so anything readable means it closed the connection.
pub(super) async fn hangup(stream: &mut UnixStream) {
    let mut closed = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(stream, &mut closed).await;
}
