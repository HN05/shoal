//! Answering a request for a space this machine has none of: one whose
//! target the hub lists on another machine before it is handled here, and
//! one refused here for naming such a space. The machine the hub lists that
//! space on answers it instead, or the hub holds it for that machine, and
//! only a space no machine has keeps the refusal; the answer of one that
//! goes on, as a follow's, comes as that machine writes it, and a duplex
//! one's connection, as a demo stream's, then carries bytes both ways. A
//! request another machine forwarded here is never forwarded again.

use anyhow::Result;
use shoal_protocol::{
    protocol::{self, Body, Forwarding, Operation, Response, Target},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use tokio::net::UnixStream;

use super::{
    Server,
    answer::{self, OperationGuard},
    hub::{Elsewhere, Unanswered},
    manager::Manager,
    relayed::Relayed,
    scope::Caller,
};
use crate::refusal::Refusal;

/// The refusal of `operation`, which goes to another machine as `routing`
/// says, for the space it targets when this machine has none of it and the
/// hub lists it on another, which answers it instead; none for a request
/// this machine answers.
pub(super) async fn targeted(
    manager: &Manager,
    routing: Option<&Routing<'_>>,
    operation: &Operation,
) -> Option<anyhow::Error> {
    let space = target_space(operation.target()?, routing?.scope)?;
    // One this machine cannot read is left to its handler's refusal.
    if manager.holds_space(&space).await.unwrap_or(true) {
        return None;
    }
    match manager.hub.machine_of(manager, &space).await {
        Ok(Some(_)) => Some(Refusal::absent(&space, format!("no space {space}")).into()),
        _ => None,
    }
}

/// The space `target` is in, for a request from `scope`; none where a
/// space's process acts in its own.
fn target_space(target: Target<'_>, scope: Option<&ScopeToken>) -> Option<SpaceSelector> {
    match target {
        Target::Space(space) => Some(space.clone()),
        Target::Item(number) => Some(SpaceSelector::Number(number.space)),
        Target::Acting(_) if scope.is_some() => None,
        Target::Acting(space) => Some(space.clone()),
    }
}

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
        if scope.is_some_and(|token| server.manager.hub.vouched_caller(token).is_some()) {
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

/// A request that failed here, which another machine may answer instead.
pub(super) struct Failed<'a> {
    pub error: &'a anyhow::Error,
    /// The answer it got here.
    pub refused: Body,
    /// How it goes to another machine.
    pub routing: Option<Routing<'a>>,
    /// Its caller, when its scope accepted it.
    pub caller: Option<Option<Caller>>,
    /// The daemon operation it holds while it is answered.
    pub operation: OperationGuard,
}

/// Answer the request `request_id`, which failed with `error`, from the
/// machine that has the space `error` names when another one does, else
/// with `refused`, the answer it got here.
pub(super) async fn answer(
    mut stream: UnixStream,
    server: &Server,
    request_id: u64,
    failed: Failed<'_>,
) -> Result<()> {
    let Failed {
        error,
        refused,
        routing,
        caller,
        operation,
    } = failed;
    let (Some(routing), Some(space)) = (routing, Refusal::absent_space(error)) else {
        return reply(&mut stream, request_id, refused).await;
    };
    let caller = match caller {
        Some(caller) => caller,
        None => scoped_caller(server, routing.scope).await?,
    };
    let scope = routing.scope;
    let unanswered = Unanswered {
        method: routing.method,
        forwarding: routing.forwarding,
        caller: caller.as_ref(),
    };
    match server
        .manager
        .hub
        .answer_elsewhere(&*server.manager, space, unanswered)
        .await
    {
        None => reply(&mut stream, request_id, refused).await,
        Some(Err(error)) => reply(&mut stream, request_id, answer::failure(&error)).await,
        Some(Ok(Elsewhere::Queued(queued))) => {
            reply(&mut stream, request_id, Body::Queued(queued)).await
        }
        Some(Ok(Elsewhere::Answered(frames))) => relay(&mut stream, request_id, frames).await,
        Some(Ok(Elsewhere::Streamed(incoming))) => {
            // It goes on, so it holds no operation, which would hold off a
            // restart meanwhile, but while it writes.
            drop(operation);
            let relayed = Relayed {
                manager: &server.manager,
                request_id,
                scope,
                held: false,
            };
            let (mut reader, mut writer) = stream.into_split();
            relayed.relay(&mut reader, &mut writer, incoming).await
        }
        Some(Ok(Elsewhere::Duplex(incoming, outgoing))) => {
            drop(operation);
            let relayed = Relayed {
                manager: &server.manager,
                request_id,
                scope,
                held: false,
            };
            relayed.relay_duplex(stream, incoming, outgoing).await
        }
    }
}

/// The caller `scope` stands for, which its refusal left unresolved.
async fn scoped_caller(server: &Server, scope: Option<&ScopeToken>) -> Result<Option<Caller>> {
    match scope {
        Some(token) => server.manager.caller(token).await,
        None => Ok(None),
    }
}

async fn reply(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    request_id: u64,
    body: Body,
) -> Result<()> {
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

#[cfg(test)]
mod tests;
