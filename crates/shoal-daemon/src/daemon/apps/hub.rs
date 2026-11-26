//! The app store across the hub, as `docs/design/apps.md` describes it. The
//! hub answers a member's request on a stream of its link with the frames
//! its own socket would write.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use shoal_apps::{Holder, Watched, Watcher, frames};
use shoal_protocol::{
    apps::store::AppScope,
    hub::{Member, link::Call},
    protocol::{AppMethod, Body, Response},
    scope_tokens::ScopeToken,
};
use tokio::net::UnixStream;

use crate::{
    daemon::{answer, manager::Manager, relayed::Relayed},
    refusal::{Reason, Refusal},
};
use shoal_hub::{
    membership,
    streams::{Incoming, Outgoing},
};

impl Manager {
    /// Whether this daemon's callers reach the shared entries through the
    /// hub: it joined one, and is not it.
    pub(crate) async fn shares_through_hub(&self) -> Result<bool> {
        if self.hub.is_hub(self) {
            return Ok(false);
        }
        Ok(self.store.read(membership::load).await?.is_some())
    }

    /// The stream the hub answers `method` on, a write with `bytes`.
    pub(crate) async fn app_request_at_hub(
        &self,
        method: AppMethod,
        bytes: Option<&[u8]>,
    ) -> Result<Incoming> {
        let data = bytes.map(|bytes| URL_SAFE_NO_PAD.encode(bytes));
        let opened = async {
            let calls = self.hub.hub_calls().await?;
            (calls.stream(|stream| Call::App {
                method,
                data,
                stream,
            }))
            .await
        };
        (opened.await).map_err(|error| Refusal::wrap(Reason::MachineUnreachable, error))
    }

    /// The hub's refusal of the write of a shared entry `method` names,
    /// before its bytes go; none once the hub would take it.
    pub(crate) async fn refused_at_hub(&self, method: AppMethod) -> Result<Option<Body>> {
        let mut incoming = self.app_request_at_hub(method, None).await?;
        let mut answer = Vec::new();
        let read = async {
            while let Some(bytes) = incoming.next().await? {
                answer.extend(bytes);
            }
            let frame = answer
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default();
            anyhow::Ok(serde_json::from_slice::<Response>(frame)?)
        };
        let response =
            (read.await).map_err(|error| Refusal::wrap(Reason::MachineUnreachable, error))?;
        Ok(match response.body {
            Body::Ok => None,
            body => Some(body),
        })
    }

    /// Answer `method`, a request of the shared entries from a caller with
    /// the scope token `scope`, on `stream` with the hub's answer as it
    /// comes; `held` while the caller holds an operation until it ends.
    pub(crate) async fn relay_from_hub(
        &self,
        stream: UnixStream,
        request_id: u64,
        scope: Option<ScopeToken>,
        method: AppMethod,
        held: bool,
    ) -> Result<()> {
        let (mut reader, mut writer) = stream.into_split();
        let incoming = match self.app_request_at_hub(method, None).await {
            Ok(incoming) => incoming,
            Err(error) => {
                let body = answer::failure(&error);
                return answer::write_frames(&mut writer, Response::new(request_id, body)).await;
            }
        };
        let relayed = Relayed {
            manager: self,
            request_id,
            scope: scope.as_ref(),
            held,
        };
        relayed.relay(&mut reader, &mut writer, incoming).await
    }

    /// As the hub, answer `member`'s `method`, a write with `data`, on
    /// `out`: a device's own entries are those `member` has.
    pub(crate) fn serve_app_request(
        self: &Arc<Self>,
        member: &Member,
        method: AppMethod,
        data: Option<String>,
        out: Outgoing,
    ) -> Result<()> {
        let holder = match method.kept_in() {
            Some(AppScope::Shared) => Holder::Shared,
            Some(AppScope::Device) => Holder::Device(member.id.clone()),
            None => anyhow::bail!("an app reads its config from the machine it runs on"),
        };
        let bytes = (data.map(|data| URL_SAFE_NO_PAD.decode(data)))
            .transpose()
            .context("a write's bytes are not base64url")?;
        let manager = self.clone();
        tokio::spawn(async move { manager.answer_on_link(holder, method, bytes, out).await });
        Ok(())
    }

    /// Write the answer to `method` of `holder`'s entries, a write of
    /// `bytes`, on `out` as this daemon's socket would, then end it.
    async fn answer_on_link(
        &self,
        holder: Holder,
        method: AppMethod,
        bytes: Option<Vec<u8>>,
        mut out: Outgoing,
    ) {
        if let AppMethod::WatchAppEntries { app, prefix, .. } = method {
            let watched = Watched {
                holder,
                app,
                prefix,
            };
            let mut link = Link { manager: self, out };
            let watched = self.apps.watch_entries(watched, &mut link).await;
            let error = watched.err().map(|error| format!("{error:#}"));
            return link.out.end(error).await;
        }
        // Held until the answer is written, so an update never exits
        // between a change and the answer that reports it.
        let Some(_operation) = self.gates.operation_unless_exiting().await else {
            let error = "the hub's daemon is restarting; try again once it is back";
            return out.end(Some(error.into())).await;
        };
        let bodies = match self.apps.answer_kept(holder, method, bytes).await {
            Ok(bodies) => bodies,
            Err(error) => vec![answer::failure(&error)],
        };
        let written = write_bodies(&mut out, bodies).await;
        out.end(written.err().map(|error| format!("{error:#}")))
            .await;
    }
}

/// Write the frames of `bodies` on `out`, as the socket writes them.
async fn write_bodies(out: &mut Outgoing, bodies: Vec<Body>) -> Result<()> {
    for body in bodies {
        out.write(&frames(body)?).await?;
    }
    Ok(())
}

/// A watch a member asked for, answered on a stream of its link.
struct Link<'a> {
    manager: &'a Manager,
    out: Outgoing,
}

impl Watcher for Link<'_> {
    async fn send(&mut self, body: Body) -> Result<bool> {
        // A restarting daemon ends the watch instead, which its watcher
        // starts again.
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Ok(false);
        };
        // Its reader stopped reading it, or the link closed.
        Ok(self.out.write_through(&frames(body)?).await.is_ok())
    }

    async fn gone(&mut self) {
        self.out.closed().await
    }
}

#[cfg(test)]
mod tests;
