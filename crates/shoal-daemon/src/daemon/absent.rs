//! Answering a request refused for naming a space this machine has none
//! of: the machine the hub lists that space on answers it instead, or the
//! hub holds it for that machine, and only a space no machine has keeps the
//! refusal; the answer of one that goes on, as a follow's, comes as that
//! machine writes it. A request another machine forwarded here is never
//! forwarded again.
use anyhow::Result;
use shoal_protocol::{
    protocol::{self, Body, Forwarding, Operation, Response},
    scope_tokens::ScopeToken,
};
use tokio::net::UnixStream;

use super::{
    Server, answer,
    hub::{Elsewhere, Incoming, Unanswered},
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
        Some(Ok(Elsewhere::Streamed(incoming))) => relay_stream(stream, request_id, incoming).await,
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

/// Write the frames another machine's daemon answers with as they come on
/// `incoming`, as the answer to `request_id`, until it ends or the client
/// hangs up.
async fn relay_stream(stream: UnixStream, request_id: u64, mut incoming: Incoming) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut frames = Frames::default();
    loop {
        let bytes = tokio::select! {
            bytes = incoming.next() => bytes,
            () = answer::hangup(&mut reader) => return Ok(()),
        };
        let bytes = match bytes {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(()),
            Err(error) => {
                let error = error.context("the other machine's answer broke off");
                return reply(&mut writer, request_id, answer::failure(&error)).await;
            }
        };
        for mut frame in frames.take(&bytes)? {
            frame["id"] = request_id.into();
            protocol::write(&mut writer, &frame).await?;
        }
    }
}

/// The frames of an answer as its bytes come, a frame per line.
#[derive(Default)]
struct Frames(Vec<u8>);

impl Frames {
    /// The frames `bytes` complete, keeping the rest for the next.
    fn take(&mut self, bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
        self.0.extend_from_slice(bytes);
        let Some(end) = self.0.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let rest = self.0.split_off(end + 1);
        let whole = std::mem::replace(&mut self.0, rest);
        let lines = whole
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty());
        Ok(lines
            .map(serde_json::from_slice)
            .collect::<serde_json::Result<_>>()?)
    }
}
