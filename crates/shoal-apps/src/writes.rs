//! Writing an entry, where a request fits in one frame and an entry need
//! not: the daemon takes the write's bytes as the frames the client sends
//! after it, once the write is checked before its bytes come.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::{
    apps::{
        AppName,
        store::{AppData, Entry, Write},
    },
    protocol::{self, Body, Response, timing::REQUEST_READ_TIMEOUT},
};
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::{Apps, Holder};

/// A write of `size` bytes as `write` to `app`'s entries.
pub struct Writing {
    pub app: AppName,
    pub write: Write,
    pub size: u64,
}

impl Apps {
    /// Check `writing` to `holder`'s entries, ask the client on `writer`
    /// for its bytes, answering the request `request_id`, read them from
    /// `reader` and write them.
    pub async fn take_write(
        &self,
        writer: &mut (impl AsyncWrite + Unpin),
        reader: &mut (impl AsyncBufRead + Unpin),
        request_id: u64,
        holder: Holder,
        writing: Writing,
    ) -> Result<Entry> {
        let Writing { app, write, size } = writing;
        (self.check_write(holder.clone(), app.clone(), write.clone(), size)).await?;
        let bytes = ask_bytes(writer, reader, request_id, size).await?;
        self.write_entry(holder, app, write, bytes).await
    }
}

/// Ask the client on `writer` for the write's `size` bytes, answering the
/// request `request_id`, and take them from `reader`.
pub async fn ask_bytes(
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
