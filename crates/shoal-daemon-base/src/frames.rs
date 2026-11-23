//! How the daemon writes an answer to its client: the frames of a reply,
//! and an answer that goes on, which holds off a restart only while it
//! writes and ends when the client hangs up.
use anyhow::Result;
use shoal_protocol::protocol::{self, Body, Response};
use tokio::{io::AsyncWrite, sync::OwnedRwLockReadGuard};

use crate::gates::Gates;

/// The guard a request holds while it runs, which keeps a daemon update from
/// restarting the daemon under it.
pub type OperationGuard = OwnedRwLockReadGuard<()>;

/// The most bytes one frame carries in base64, with room for the rest of
/// the frame.
pub const CHUNK: usize = (protocol::MAX_FRAME - (8 << 10)) / 4 * 3;

/// Write `response` in the frames it takes.
pub async fn write_frames(
    stream: &mut (impl AsyncWrite + Unpin),
    response: Response,
) -> Result<()> {
    for frame in response.frames()? {
        protocol::write(stream, &frame).await?;
    }
    Ok(())
}

/// Where an answer that goes on writes its frames: its client's
/// connection.
pub struct Frames<'a> {
    gates: &'a Gates,
    writer: tokio::net::unix::OwnedWriteHalf,
    id: u64,
}

impl<'a> Frames<'a> {
    /// The frames answering the request `id` on `writer`, under the
    /// daemon's `gates`.
    pub fn new(gates: &'a Gates, writer: tokio::net::unix::OwnedWriteHalf, id: u64) -> Self {
        Self { gates, writer, id }
    }

    /// Send `body` under a daemon operation of its own, so the answer holds
    /// off a restart only while it writes; `false` once a restart holds
    /// operations off, which closes the answer.
    pub async fn send(&mut self, body: Body) -> Result<bool> {
        let Some(_operation) = self.gates.try_operation() else {
            return Ok(false);
        };
        protocol::write(&mut self.writer, &Response::new(self.id, body)).await?;
        Ok(true)
    }
}

/// Wait until the client hangs up. A client sends nothing after its request,
/// so anything readable means it closed the connection.
pub async fn hangup(stream: &mut (impl tokio::io::AsyncRead + Unpin)) {
    let mut closed = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(stream, &mut closed).await;
}
