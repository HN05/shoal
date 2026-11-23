//! The app store on the daemon's socket. A write's bytes come as the frames
//! the client sends after the request, and the daemon answers with the
//! entry at its new revision; the hub checks and takes the write of a
//! shared entry for a machine that joined one. A watch holds an operation
//! guard only while it writes, so a daemon restart closes it between writes
//! and the client watches again.
use std::sync::Arc;

use anyhow::Result;
use shoal_apps::{Holder, Watcher, Writing, ask_bytes};
use shoal_protocol::{
    apps::store::AppScope,
    protocol::{AppMethod, Body, ErrorCode, Response},
    scope_tokens::ScopeToken,
};
use tokio::{
    io::{AsyncBufRead, AsyncWrite, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

use crate::daemon::{answer, manager::Manager, relayed::Relayed};

/// Who keeps the entries a request reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Keeper {
    /// This daemon, as `holder`'s.
    Here(Holder),
    /// The hub this machine joined, the shared entries'.
    Hub,
}

impl Manager {
    /// Answer the write `request_id` of `writing` to the entries `keeper`
    /// keeps, from a caller with the scope token `scope`, on `stream`: ask
    /// for its bytes once it would be taken, then write them.
    pub(crate) async fn answer_write(
        &self,
        stream: UnixStream,
        request_id: u64,
        (keeper, writing): (Keeper, Writing),
        scope: Option<ScopeToken>,
    ) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let holder = match keeper {
            Keeper::Here(holder) => holder,
            Keeper::Hub => {
                let writing = (writing, scope.as_ref());
                let at_hub = self.write_at_hub(&mut writer, &mut reader, request_id, writing);
                return at_hub.await;
            }
        };
        let taken = (self.apps).take_write(&mut writer, &mut reader, request_id, holder, writing);
        let body = match taken.await {
            Ok(entry) => Body::AppEntry(entry),
            Err(error) => answer::failure(&error),
        };
        answer::write_frames(&mut writer, Response::new(request_id, body)).await
    }

    /// Take `writing`'s bytes from the client and send them to the hub,
    /// whose answer goes back to the client as it comes.
    async fn write_at_hub(
        &self,
        writer: &mut (impl AsyncWrite + Unpin),
        reader: &mut (impl AsyncBufRead + Unpin),
        request_id: u64,
        (writing, scope): (Writing, Option<&ScopeToken>),
    ) -> Result<()> {
        let Writing { app, write, size } = writing;
        let method = AppMethod::WriteAppEntry {
            app,
            scope: AppScope::Shared,
            write,
            size,
        };
        let sent = async {
            // The hub checks the write before its bytes come, and again as
            // it takes them.
            if let Some(refusal) = self.refused_at_hub(method.clone()).await? {
                return Ok(Err(refusal));
            }
            let bytes = ask_bytes(writer, reader, request_id, size).await?;
            (self.app_request_at_hub(method, Some(&bytes)).await).map(Ok)
        };
        let incoming = match sent.await {
            Ok(Ok(incoming)) => incoming,
            Ok(Err(refusal)) => {
                return answer::write_frames(writer, Response::new(request_id, refusal)).await;
            }
            Err(error) => {
                let body = answer::failure(&error);
                return answer::write_frames(writer, Response::new(request_id, body)).await;
            }
        };
        let relayed = Relayed {
            manager: self,
            request_id,
            scope,
            held: true,
        };
        relayed.relay(reader, writer, incoming).await
    }
}

/// A watch on the daemon's socket: the request it answers, and the scope
/// token it came with, which must stay valid while it runs.
pub(crate) struct Socket {
    pub manager: Arc<Manager>,
    pub reader: OwnedReadHalf,
    pub writer: OwnedWriteHalf,
    pub request_id: u64,
    pub scope: Option<ScopeToken>,
}

impl Socket {
    pub(crate) fn new(
        manager: Arc<Manager>,
        stream: UnixStream,
        request_id: u64,
        scope: Option<ScopeToken>,
    ) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            manager,
            reader,
            writer,
            request_id,
            scope,
        }
    }

    async fn write(&mut self, body: Body) -> Result<()> {
        answer::write_frames(&mut self.writer, Response::new(self.request_id, body)).await
    }
}

impl Watcher for Socket {
    async fn send(&mut self, body: Body) -> Result<bool> {
        // A restarting daemon closes the watch instead, and the client
        // watches again on its replacement.
        let manager = self.manager.clone();
        let Some(_operation) = manager.gates.try_operation() else {
            return Ok(false);
        };
        if !manager.still_scoped(self.scope.as_ref()).await? {
            let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
            self.write(refused).await?;
            return Ok(false);
        }
        self.write(body).await?;
        Ok(true)
    }

    async fn gone(&mut self) {
        answer::hangup(&mut self.reader).await
    }
}
