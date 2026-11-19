//! The owning machine's side of a forwarded request: its daemon answers it
//! through its own socket, as it answers its own callers' requests, under a
//! scope token that stands for the caller the forwarding machine vouches
//! for until the answer is read. The user's requests go as the user's; a
//! space's go under that space's scope, which reaches what that space's
//! reaches on every machine, with the permissions its machine vouches it
//! holds. One whose answer goes on, as a follow's, goes on a stream as the
//! daemon writes it. A forwarded request is never forwarded again.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::routing::{Answered, Forwarded, Vouched, VouchedSpace},
    protocol::{self, Forwarding, Method, Request},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};

use super::streams::Outgoing;
use crate::daemon::{scope::Caller, workspace::Manager};

/// The most bytes of a streamed answer read at once.
const READ_BYTES: usize = 64 << 10;

/// The callers that forwarded requests stand for while this machine
/// answers them, by the scope token each request carries.
#[derive(Default)]
pub(crate) struct Vouches(Mutex<HashMap<ScopeToken, Vouched>>);

/// A token standing for a vouched caller, which stops standing for it once
/// dropped.
struct Held<'a> {
    vouches: &'a Vouches,
    token: ScopeToken,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.vouches.lock().remove(&self.token);
    }
}

impl Vouches {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ScopeToken, Vouched>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new token standing for `caller` until the returned hold drops.
    fn hold(&self, caller: Vouched) -> Held<'_> {
        let token = crate::daemon::scope::new_token();
        self.lock().insert(token.clone(), caller);
        Held {
            vouches: self,
            token,
        }
    }

    /// The caller `token` stands for, while its forwarded request runs.
    pub(crate) fn caller(&self, token: &ScopeToken) -> Option<Vouched> {
        self.lock().get(token).cloned()
    }

    /// What the machine of the space `id` vouches for it, while a request
    /// its process forwarded runs here.
    pub(crate) fn space(&self, id: &SpaceId) -> Option<VouchedSpace> {
        self.lock().values().find_map(|vouched| match vouched {
            Vouched::Space(space) if &space.space == id => Some(space.clone()),
            _ => None,
        })
    }
}

impl Manager {
    /// Answer `request`, which another machine forwarded, as this daemon
    /// answers its own callers, refusing one that only the space's own
    /// machine runs, since it needs files or processes the caller's has
    /// not.
    pub(super) async fn serve_forwarded(&self, request: Forwarded) -> Result<Answered> {
        let Forwarded { caller, method, .. } = request;
        let method: Method =
            serde_json::from_value(method).context("unreadable forwarded request")?;
        // One whose answer goes on comes on a stream; collected here, it
        // would never end.
        match &method {
            Method::Operation(operation)
                if matches!(
                    operation.forwarding(),
                    Forwarding::Online | Forwarding::Queued
                ) => {}
            Method::Operation(operation) if operation.forwarding() == Forwarding::Streamed => {
                bail!("this request's answer goes on, so it comes only on a stream")
            }
            _ => bail!("this request runs only on the machine of the space it acts on"),
        }
        let held = self.hub.vouches.hold(caller);
        let request = Request {
            scope: Some(held.token.clone()),
            ..Request::new(method)
        };
        let frames = self.answer_on_socket(&request).await;
        drop(held);
        Ok(Answered { frames: frames? })
    }

    /// Answer `request`, another machine's request whose answer goes on,
    /// as this daemon answers its own callers, writing it to `out` as the
    /// daemon writes it until either side ends; refused for any other
    /// request.
    pub(super) fn serve_streamed(
        self: &Arc<Self>,
        request: Forwarded,
        out: Outgoing,
    ) -> Result<()> {
        let Forwarded { caller, method, .. } = request;
        let method: Method =
            serde_json::from_value(method).context("unreadable forwarded request")?;
        match &method {
            Method::Operation(operation) if operation.forwarding() == Forwarding::Streamed => {}
            _ => bail!("only a request whose answer goes on is answered on a stream"),
        }
        let manager = self.clone();
        tokio::spawn(async move { manager.stream_from_socket(caller, method, out).await });
        Ok(())
    }

    /// Write what this daemon answers `method` from `caller` with on its
    /// own socket to `out`, ending it as the answer ends, until no one
    /// reads `out` any longer. The token stands for `caller` only until
    /// the answer starts, once the request was decided: what its machine
    /// vouched then would otherwise outlast what that machine vouches for
    /// its later requests, which it checks against the scope as the answer
    /// goes on.
    async fn stream_from_socket(&self, caller: Vouched, method: Method, mut out: Outgoing) {
        let mut held = Some(self.hub.vouches.hold(caller));
        let request = Request {
            scope: held.as_ref().map(|held| held.token.clone()),
            ..Request::new(method)
        };
        let streamed = async {
            let mut stream = tokio::net::UnixStream::connect(&self.paths.socket)
                .await
                .context("could not reach this machine's daemon socket")?;
            protocol::write(&mut stream, &request).await?;
            let mut bytes = vec![0; READ_BYTES];
            loop {
                let read = tokio::select! {
                    read = stream.read(&mut bytes) => read?,
                    () = out.closed() => return Ok(()),
                };
                if read == 0 {
                    return Ok(());
                }
                held = None;
                out.write(&bytes[..read]).await?;
            }
        };
        let streamed: Result<()> = streamed.await;
        drop(held);
        out.end(streamed.err().map(|error| format!("{error:#}")))
            .await;
    }

    /// The frames this daemon writes in answer to `request` on its own
    /// socket, until it closes the connection.
    async fn answer_on_socket(&self, request: &Request) -> Result<Vec<serde_json::Value>> {
        let mut stream = tokio::net::UnixStream::connect(&self.paths.socket)
            .await
            .context("could not reach this machine's daemon socket")?;
        protocol::write(&mut stream, request).await?;
        let mut reader = BufReader::new(stream);
        let mut frames = Vec::new();
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).await? == 0 {
                return Ok(frames);
            }
            frames.push(serde_json::from_slice(&line).context("unreadable response frame")?);
        }
    }

    /// What another machine vouches for the space `id`, a space there,
    /// while a request its process forwarded runs here.
    pub(crate) fn vouched_space(&self, id: &SpaceId) -> Option<VouchedSpace> {
        self.hub.vouches.space(id)
    }

    /// Whether `selector` names `caller`'s own space where another machine
    /// vouches for it, which this machine has none of.
    pub(crate) fn names_vouched_own(&self, caller: &Caller, selector: &SpaceSelector) -> bool {
        selector.given_id() == Some(&caller.space_id)
            && self.vouched_space(&caller.space_id).is_some()
    }

    /// The ID and name of the space `selector` names: one here, or the
    /// space another machine vouches for while its process's forwarded
    /// request runs here, by its ID.
    pub(crate) async fn acting_space(&self, selector: &SpaceSelector) -> Result<(SpaceId, String)> {
        if let Some(id) = selector.given_id()
            && self.vouched_space(id).is_some()
        {
            return Ok((id.clone(), id.to_string()));
        }
        let space = self.space(selector).await?;
        Ok((space.id, space.name.to_string()))
    }

    /// The caller a forwarded request's `token` stands for: none for the
    /// user's, else the vouched space's process; `None` for a token no
    /// forwarded request holds.
    pub(crate) fn vouched_caller(&self, token: &ScopeToken) -> Option<Option<Caller>> {
        Some(match self.hub.vouches.caller(token)? {
            Vouched::User => None,
            Vouched::Space(space) => Some(Caller {
                granted: space.granted,
                read_only: space.read_only,
                ..Caller::of_space(space.space)
            }),
        })
    }
}

#[cfg(test)]
mod tests;
