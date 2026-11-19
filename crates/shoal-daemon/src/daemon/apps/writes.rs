//! Writing an entry on the daemon's socket, where a request fits in one
//! frame and an entry need not: the daemon takes the write's bytes as the
//! frames the client sends after it, and answers with the entry at its new
//! revision. The write is checked before its bytes come, by the hub for a
//! shared entry of a machine that joined one, which then takes the bytes
//! and answers.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::{
    apps::{
        AppName,
        store::{AppData, AppScope, Entry, Write},
    },
    protocol::{self, AppMethod, Body, Response, timing::REQUEST_READ_TIMEOUT},
    scope_tokens::ScopeToken,
};
use tokio::{
    io::{AsyncBufRead, AsyncWrite, BufReader},
    net::UnixStream,
};

use super::entries::Holder;
use crate::daemon::{answer, manager::Manager, relayed::Relayed};

/// Who keeps the entries a request reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Keeper {
    /// This daemon, as `holder`'s.
    Here(Holder),
    /// The hub this machine joined, the shared entries'.
    Hub,
}

/// A write of `size` bytes as `write` to `app`'s entries that `keeper`
/// keeps, from a caller with the scope token `scope`.
pub(crate) struct Writing {
    pub keeper: Keeper,
    pub app: AppName,
    pub write: Write,
    pub size: u64,
    pub scope: Option<ScopeToken>,
}

impl Manager {
    /// Answer the write `request_id` on `stream`: ask for its bytes once
    /// it would be taken, then write them.
    pub(crate) async fn answer_write(
        &self,
        stream: UnixStream,
        request_id: u64,
        writing: Writing,
    ) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let written = match writing.keeper.clone() {
            Keeper::Here(holder) => {
                (self.write_here(&mut writer, &mut reader, request_id, holder, writing)).await
            }
            Keeper::Hub => {
                let at_hub = self.write_at_hub(&mut writer, &mut reader, request_id, writing);
                return at_hub.await;
            }
        };
        let body = match written {
            Ok(entry) => Body::AppEntry(entry),
            Err(error) => answer::failure(&error),
        };
        answer::write_frames(&mut writer, Response::new(request_id, body)).await
    }

    /// Check `writing` to `holder`'s entries, ask the client on `writer`
    /// for its bytes, read them from `reader` and write them.
    async fn write_here(
        &self,
        writer: &mut (impl AsyncWrite + Unpin),
        reader: &mut (impl AsyncBufRead + Unpin),
        request_id: u64,
        holder: Holder,
        writing: Writing,
    ) -> Result<Entry> {
        let Writing {
            app, write, size, ..
        } = writing;
        (self.check_kept_write(holder.clone(), app.clone(), write.clone(), size)).await?;
        let bytes = ask_bytes(writer, reader, request_id, size).await?;
        self.write_kept_entry(holder, app, write, bytes).await
    }

    /// Take `writing`'s bytes from the client and send them to the hub,
    /// whose answer goes back to the client as it comes.
    async fn write_at_hub(
        &self,
        writer: &mut (impl AsyncWrite + Unpin),
        reader: &mut (impl AsyncBufRead + Unpin),
        request_id: u64,
        writing: Writing,
    ) -> Result<()> {
        let Writing {
            app,
            write,
            size,
            scope,
            ..
        } = writing;
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
            scope: scope.as_ref(),
            held: true,
        };
        relayed.relay(reader, writer, incoming).await
    }
}

/// Ask the client on `writer` for the write's `size` bytes, and take them
/// from `reader`.
async fn ask_bytes(
    writer: &mut (impl AsyncWrite + Unpin),
    reader: &mut (impl AsyncBufRead + Unpin),
    request_id: u64,
    size: u64,
) -> Result<Vec<u8>> {
    protocol::write(writer, &Response::new(request_id, Body::Ok)).await?;
    receive(reader, size).await
}

/// The `size` bytes a client sends as [`AppData`] frames, each within the
/// time a request takes to arrive.
async fn receive(reader: &mut (impl AsyncBufRead + Unpin), size: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < size {
        let piece = tokio::time::timeout(REQUEST_READ_TIMEOUT, protocol::read_buffered(reader));
        let piece: AppData = piece
            .await
            .context("the client stopped sending the entry's bytes")??;
        bytes.extend(piece.bytes()?);
        ensure!(
            bytes.len() as u64 <= size,
            "the client sent more than the {size} bytes it named"
        );
    }
    Ok(bytes)
}
