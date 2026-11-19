//! Writing an entry on the daemon's socket, where a request fits in one
//! frame and an entry need not: the daemon checks the write first, then
//! takes its bytes as the frames the client sends after it, and answers
//! with the entry at its new revision.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::{
    apps::{
        AppName,
        store::{AppData, Entry, Write},
    },
    protocol::{self, Body, Response, timing::REQUEST_READ_TIMEOUT},
};
use tokio::{
    io::{AsyncBufRead, BufReader},
    net::UnixStream,
};

use super::entries::Holder;
use crate::daemon::{answer, manager::Manager};

/// A write of `size` bytes as `write` to `app`'s entries that `holder`
/// has.
pub(crate) struct Writing {
    pub holder: Holder,
    pub app: AppName,
    pub write: Write,
    pub size: u64,
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
        let written = self.take_write(&mut writer, &mut reader, request_id, writing);
        let body = match written.await {
            Ok(entry) => Body::AppEntry(entry),
            Err(error) => answer::failure(&error),
        };
        answer::write_frames(&mut writer, Response::new(request_id, body)).await
    }

    /// Check `writing`, ask the client on `writer` for its bytes, read
    /// them from `reader` and write them.
    async fn take_write(
        &self,
        writer: &mut (impl tokio::io::AsyncWrite + Unpin),
        reader: &mut (impl AsyncBufRead + Unpin),
        request_id: u64,
        writing: Writing,
    ) -> Result<Entry> {
        let Writing {
            holder,
            app,
            write,
            size,
        } = writing;
        (self.check_kept_write(holder.clone(), app.clone(), write.clone(), size)).await?;
        protocol::write(writer, &Response::new(request_id, Body::Ok)).await?;
        let bytes = receive(reader, size).await?;
        self.write_kept_entry(holder, app, write, bytes).await
    }
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
