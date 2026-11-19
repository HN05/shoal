//! Answering a request with the frames another daemon writes as they come
//! on a stream of the hub link, as that daemon's socket would answer it,
//! under the request's own ID, or on a duplex stream with the connection's
//! bytes both ways after the answer.
use anyhow::Result;
use shoal_protocol::{
    protocol::{self, Body, ErrorCode, Response},
    scope_tokens::ScopeToken,
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{answer, hub::Incoming, manager::Manager};

mod duplex;
use crate::refusal::{Reason, Refusal};

/// How often a relayed answer checks that its caller's scope still holds.
const SCOPE_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// A request whose answer another daemon writes.
pub(super) struct Relayed<'a> {
    pub manager: &'a Manager,
    pub request_id: u64,
    /// The scope token it came with.
    pub scope: Option<&'a ScopeToken>,
    /// Whether its caller holds an operation until it ends, as an answer
    /// that reports a change does, so it takes none of its own.
    pub held: bool,
}

impl Relayed<'_> {
    /// Write the frames another daemon answers with as they come on
    /// `incoming` to `writer`, until it ends, the client hangs up on
    /// `reader`, the request's scope token stops acting for a caller, or a
    /// restart closes it.
    pub(super) async fn relay(
        &self,
        reader: &mut (impl AsyncRead + Unpin),
        writer: &mut (impl AsyncWrite + Unpin),
        mut incoming: Incoming,
    ) -> Result<()> {
        let mut frames = Frames::default();
        let mut scope_check = tokio::time::interval(SCOPE_CHECK);
        loop {
            let bytes = tokio::select! {
                bytes = incoming.next() => bytes,
                _ = scope_check.tick() => {
                    if self.manager.still_scoped(self.scope).await? {
                        continue;
                    }
                    return self.refuse_scope(writer).await;
                }
                () = answer::hangup(reader) => return Ok(()),
            };
            let bytes = match bytes {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return Ok(()),
                Err(error) => {
                    let error = error.context("the other machine's answer broke off");
                    let error = Refusal::wrap(Reason::MachineUnreachable, error);
                    let failure = body_frame(self.request_id, answer::failure(&error))?;
                    return self.write(writer, vec![failure]).await.map(drop);
                }
            };
            // Its client gets nothing more once its scope token stops
            // acting, as while it paused reading.
            if !self.manager.still_scoped(self.scope).await? {
                return self.refuse_scope(writer).await;
            }
            if !self.write(writer, frames.take(&bytes)?).await? {
                return Ok(());
            }
        }
    }

    /// End the answer, whose scope token no longer acts for a caller.
    async fn refuse_scope(&self, writer: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let body = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
        let refusal = vec![body_frame(self.request_id, body)?];
        self.write(writer, refusal).await.map(drop)
    }

    /// Write `frames` as the answer's, under a daemon operation; `false`
    /// once a restart holds operations off, which closes the answer for
    /// its client to ask for again.
    async fn write(
        &self,
        writer: &mut (impl AsyncWrite + Unpin),
        frames: Vec<serde_json::Value>,
    ) -> Result<bool> {
        let _operation = match self.held {
            true => None,
            false => match self.manager.gates.try_operation() {
                Some(operation) => Some(operation),
                None => return Ok(false),
            },
        };
        for mut frame in frames {
            frame["id"] = self.request_id.into();
            protocol::write(writer, &frame).await?;
        }
        Ok(true)
    }
}

/// The frame of `body` in answer to `request_id`.
fn body_frame(request_id: u64, body: Body) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(Response::new(request_id, body))?)
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
