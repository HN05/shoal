//! The owning machine's side of a forwarded request, which its daemon
//! answers under a scope token for the caller the forwarding machine vouches
//! for. A forwarded request is never forwarded again.
use shoal_daemon_base::callers::{Access, Caller};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::routing::{Answered, Forwarded, Vouched},
    protocol::{Forwarding, Method, Request},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};

use super::streams::{Incoming, Outgoing, Streams};
use crate::{Hub, daemon::Daemon};

/// The callers that forwarded requests stand for while this machine
/// answers them, by the scope token each request carries.
#[derive(Default)]
pub(crate) struct Vouches(Mutex<HashMap<ScopeToken, Vouched>>);

/// A token standing for a vouched caller, which stops standing for it once
/// dropped.
pub struct Held<'a> {
    vouches: &'a Vouches,
    pub token: ScopeToken,
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
        let token = ScopeToken::generate();
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
}

/// What the daemon's tests of serving read of the vouches.
#[cfg(any(test, feature = "test-support"))]
impl Hub {
    /// A token standing for `caller` until the returned hold drops, as a
    /// forwarded request's does.
    pub fn hold_vouch(&self, caller: Vouched) -> Held<'_> {
        self.vouches.hold(caller)
    }

    /// How many vouched callers tokens stand for now.
    pub fn vouches_held(&self) -> usize {
        self.vouches.lock().len()
    }
}

impl Hub {
    /// Answer `request`, which another machine forwarded, as this daemon
    /// answers its own callers, refusing one that needs the caller's own
    /// machine.
    pub async fn serve_forwarded(
        &self,
        daemon: &dyn Daemon,
        request: Forwarded,
    ) -> Result<Answered> {
        let Forwarded { caller, method, .. } = request;
        let method: Method =
            serde_json::from_value(method).context("unreadable forwarded request")?;
        // One whose answer goes on comes on a stream; collected here, it
        // would never end.
        match &method {
            Method::Operation(operation)
                if matches!(
                    operation.forwarding(),
                    Forwarding::Online | Forwarding::Reported | Forwarding::Queued
                ) => {}
            Method::Operation(operation)
                if matches!(
                    operation.forwarding(),
                    Forwarding::Streamed | Forwarding::Duplex
                ) =>
            {
                bail!("this request's answer goes on, so it comes only on a stream")
            }
            _ => bail!("this request runs only on the machine of the space it acts on"),
        }
        let held = self.vouches.hold(caller);
        let request = Request {
            scope: Some(held.token.clone()),
            ..Request::new(method)
        };
        let frames = daemon.answer(request).await;
        drop(held);
        Ok(Answered { frames: frames? })
    }

    /// Answer `request`, another machine's request whose answer goes on, as
    /// this daemon answers its own callers. Writes the answer on the stream
    /// `stream` as the daemon writes it, until either side ends. On a duplex
    /// stream, it passes on what the caller writes there. Any other request is
    /// refused.
    pub fn serve_streamed(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        request: Forwarded,
        (streams, stream): (&Streams, u64),
    ) -> Result<()> {
        let Forwarded { caller, method, .. } = request;
        let method: Method =
            serde_json::from_value(method).context("unreadable forwarded request")?;
        let ends = streams.answering(stream, duplex_method(&method)?);
        let (hub, daemon) = (self.clone(), daemon.clone());
        tokio::spawn(async move { hub.stream_answer(&*daemon, caller, method, ends).await });
        Ok(())
    }

    /// Write what this daemon answers `method` from `caller` with to `out`, and
    /// end it as the answer ends or once no one reads `out`. Pass what comes
    /// `up` to the daemon.
    ///
    /// The token stands for `caller` only until the answer starts, once the
    /// request was decided. Otherwise what the caller's machine vouched then
    /// would outlast what it vouches for its later requests. The daemon checks
    /// those against the scope as the answer goes on.
    async fn stream_answer(
        &self,
        daemon: &dyn Daemon,
        caller: Vouched,
        method: Method,
        (mut out, up): (Outgoing, Option<Incoming>),
    ) {
        let held = self.vouches.hold(caller);
        let request = Request {
            scope: Some(held.token.clone()),
            ..Request::new(method)
        };
        let started = Box::new(move || drop(held));
        let streamed = daemon.stream(request, &mut out, up, started).await;
        out.end(streamed.err().map(|error| format!("{error:#}")))
            .await;
    }

    /// Whether `selector` names `caller`'s own space where another machine
    /// vouches for it, which this machine has none of.
    pub fn names_vouched_own(&self, caller: &Caller, selector: &SpaceSelector) -> bool {
        caller.vouched.is_some() && selector.given_id() == Some(&caller.space_id)
    }

    /// The ID and name of the space `selector` names: one here, or
    /// `caller`'s own where another machine vouches for it, by its ID.
    pub async fn acting_space(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<(SpaceId, String)> {
        if let Some(caller) = caller
            && self.names_vouched_own(caller, selector)
        {
            return Ok((caller.space_id.clone(), caller.space_id.to_string()));
        }
        let space = daemon.select_space(selector).await?;
        Ok((space.id, space.name.to_string()))
    }

    /// The caller a forwarded request's `token` stands for: none for the
    /// user's, else the vouched space's process; `None` for a token no
    /// forwarded request holds.
    pub fn vouched_caller(&self, token: &ScopeToken) -> Option<Option<Caller>> {
        Some(match self.vouches.caller(token)? {
            Vouched::User => None,
            Vouched::Space(space) => Some(Caller {
                space_id: space.space.clone(),
                execution: None,
                granted: space.granted,
                access: match space.read_only {
                    true => Access::ReadOnly,
                    false => Access::Full,
                },
                vouched: Some(Arc::new(space)),
            }),
        })
    }
}

/// Whether the forwarded `request` goes on a duplex stream; refused for
/// one whose answer comes on no stream.
pub(super) fn duplex(request: &Forwarded) -> Result<bool> {
    let method: Method =
        serde_json::from_value(request.method.clone()).context("unreadable forwarded request")?;
    duplex_method(&method)
}

/// Whether `method` goes on a duplex stream; refused for one whose answer
/// comes on no stream.
fn duplex_method(method: &Method) -> Result<bool> {
    let forwarding = match method {
        Method::Operation(operation) => operation.forwarding(),
        _ => Forwarding::Never,
    };
    match forwarding {
        Forwarding::Streamed => Ok(false),
        Forwarding::Duplex => Ok(true),
        _ => bail!("only a request whose answer goes on is answered on a stream"),
    }
}
