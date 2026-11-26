//! How the daemon answers a request. A request's handler chooses the shape
//! of its answer; sending it is the same for every request.
use anyhow::Result;
use futures_util::{FutureExt, future::BoxFuture};
use tokio::{io::AsyncWrite, net::UnixStream};

use shoal_protocol::hub::routing::Queued;

use super::{approvals::Undecided, manager::Manager};
use crate::{
    protocol::{self, Body, ErrorCode, Response},
    refusal::Reason,
};

#[cfg(test)]
mod tests;

pub(crate) use shoal_daemon_base::frames::{OperationGuard, hangup, write_frames};

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
    /// A listing that failed, whose error a request for a space another
    /// machine has goes on with.
    Failed(anyhow::Error),
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
            Err(error) => Self::Failed(error),
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

    /// The answer as the machine that forwarded its request reads it: the
    /// attached files it lists are kept on `machine`, this one.
    pub(super) fn files_kept_on(self, machine: String) -> Self {
        let kept = move |mut body: Body| {
            body.files_kept_on(&machine);
            body
        };
        match self {
            Self::Body(body) => Self::Body(kept(body)),
            Self::Pages(pages) => {
                Self::Pages(pages.map(|pages| pages.into_iter().map(kept).collect()))
            }
            Self::Wait(wait) => Self::wait(async move { wait.await.map(kept) }),
            answer @ (Self::Connection(_) | Self::Failed(_)) => answer,
        }
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
            Self::Failed(error) => reply(&mut stream, request_id, failure(&error)).await,
        }
    }
}

/// The error body of an operation that failed with `error`: the approval
/// request it waits for or was refused by, or what the hub holds for an
/// offline machine, else its code and message.
pub(super) fn failure(error: &anyhow::Error) -> Body {
    if let Some(Undecided(request)) = error.downcast_ref::<Undecided>() {
        return Body::Undecided(request.clone());
    }
    match error.downcast_ref::<Queued>() {
        Some(queued) => Body::Queued(queued.clone()),
        None => Body::error(error_code(error), format!("{error:#}")),
    }
}

/// The reply to a request its caller's scope refused with `error`: a
/// target that does not exist is refused as such, not as outside the scope,
/// and an action a permission holds back for approval answers with the
/// request.
pub(super) fn refusal(error: &anyhow::Error) -> Body {
    match Reason::of(error) {
        _ if error.downcast_ref::<Undecided>().is_some() => failure(error),
        Some(Reason::NotFound) => Body::error(ErrorCode::NotFound, format!("{error:#}")),
        _ => Body::error(ErrorCode::ScopeDenied, format!("{error:#}")),
    }
}

/// The code of an operation's failure: specific where the caller can act on
/// it, otherwise `operation_failed`.
fn error_code(error: &anyhow::Error) -> ErrorCode {
    if error
        .downcast_ref::<super::executions::UnprovenStop>()
        .is_some()
    {
        ErrorCode::UnprovenStop
    } else if (error.downcast_ref::<super::agent_queue::HasStoppedAgent>()).is_some() {
        ErrorCode::StoppedAgent
    } else if let Some(exiting) = error.downcast_ref::<super::gates::Exiting>() {
        exiting.code()
    } else if error.downcast_ref::<super::scope::ScopeRefusal>().is_some() {
        ErrorCode::ScopeDenied
    } else {
        Reason::of(error).map_or(ErrorCode::OperationFailed, Reason::code)
    }
}

/// Write the reply to `request_id` in the frames it takes: a list in pages,
/// another reply too large for one frame in pieces of its JSON. Should a
/// frame still not fit, the client gets an error saying so, and the reply
/// fails the connection, for the log.
pub(super) async fn reply(
    stream: &mut (impl AsyncWrite + Unpin),
    request_id: u64,
    body: Body,
) -> Result<()> {
    let Err(error) = write_frames(stream, Response::new(request_id, body)).await else {
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
    let body = result.unwrap_or_else(|error| Body::error(error_code(&error), format!("{error:#}")));
    // Hold the operation guard through the write so a restart cannot begin
    // while the reply is in flight.
    let Some(_operation) = manager.gates.try_operation() else {
        return Ok(());
    };
    write_frames(&mut stream, Response::new(request_id, body)).await
}
