//! Answering a request refused for naming a space this machine has none
//! of: the machine the hub lists that space on answers it instead, or the
//! hub holds it for that machine, and only a space no machine has keeps the
//! refusal. A request another machine forwarded here is never forwarded
//! again.
use anyhow::Result;
use shoal_protocol::{
    protocol::{self, Body, Forwarding, Operation, Response},
    scope_tokens::ScopeToken,
};
use tokio::net::UnixStream;

use super::{
    Server, answer,
    hub::{Elsewhere, Unanswered},
    scope::Caller,
};
use crate::refusal::Refusal;

/// What a request for a space elsewhere goes on with: its method as its
/// caller sent it, before its scope narrowed it, and its scope token.
pub(super) struct Routing<'a> {
    method: Option<serde_json::Value>,
    forwarding: Forwarding,
    scope: Option<&'a ScopeToken>,
}

impl<'a> Routing<'a> {
    /// How `operation`, sent under `scope`, goes to another machine; none
    /// for a request another machine forwarded here.
    pub(super) fn of(
        server: &Server,
        operation: &Operation,
        scope: Option<&'a ScopeToken>,
    ) -> Result<Option<Self>> {
        if scope.is_some_and(|token| server.manager.vouched_caller(token).is_some()) {
            return Ok(None);
        }
        let forwarding = operation.forwarding();
        let method = match forwarding {
            Forwarding::Never => None,
            _ => Some(serde_json::to_value(operation)?),
        };
        Ok(Some(Self {
            method,
            forwarding,
            scope,
        }))
    }
}

/// Answer the request `request_id`, which failed with `error`, from the
/// machine that has the space `error` names when another one does, else
/// with `refused`, the answer it got here. `caller` is the request's,
/// when its scope accepted it.
pub(super) async fn answer(
    mut stream: UnixStream,
    server: &Server,
    request_id: u64,
    (error, refused): (&anyhow::Error, Body),
    routing: Option<Routing<'_>>,
    caller: Option<Option<Caller>>,
) -> Result<()> {
    let (Some(routing), Some(space)) = (routing, Refusal::absent_space(error)) else {
        return reply(&mut stream, request_id, refused).await;
    };
    let caller = match caller {
        Some(caller) => caller,
        None => scoped_caller(server, routing.scope).await?,
    };
    let unanswered = Unanswered {
        method: routing.method,
        forwarding: routing.forwarding,
        caller: caller.as_ref(),
    };
    match server.manager.answer_elsewhere(space, unanswered).await {
        None => reply(&mut stream, request_id, refused).await,
        Some(Err(error)) => reply(&mut stream, request_id, answer::failure(&error)).await,
        Some(Ok(Elsewhere::Queued(queued))) => {
            reply(&mut stream, request_id, Body::Queued(queued)).await
        }
        Some(Ok(Elsewhere::Answered(frames))) => relay(&mut stream, request_id, frames).await,
    }
}

/// The caller `scope` stands for, which its refusal left unresolved.
async fn scoped_caller(server: &Server, scope: Option<&ScopeToken>) -> Result<Option<Caller>> {
    match scope {
        Some(token) => server.manager.caller(token).await,
        None => Ok(None),
    }
}

async fn reply(stream: &mut UnixStream, request_id: u64, body: Body) -> Result<()> {
    protocol::write(stream, &Response::new(request_id, body)).await
}

/// Write the frames another machine's daemon answered with, as the answer
/// to `request_id`.
async fn relay(
    stream: &mut UnixStream,
    request_id: u64,
    frames: Vec<serde_json::Value>,
) -> Result<()> {
    for mut frame in frames {
        frame["id"] = request_id.into();
        protocol::write(stream, &frame).await?;
    }
    Ok(())
}
