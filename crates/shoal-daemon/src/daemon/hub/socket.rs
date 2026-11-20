//! Answering what another machine forwarded here on this daemon's own
//! socket, as the daemon answers its own callers' requests: collecting the
//! frames of an answer, or writing one that goes on to the hub's stream as
//! it comes and passing what the caller writes back the other way.
use std::path::Path;

use anyhow::{Context as _, Result};
use shoal_hub::streams::{Incoming, Outgoing};
use shoal_protocol::protocol::{self, Request};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

/// The most bytes of a streamed answer read at once.
const READ_BYTES: usize = 64 << 10;

/// The frames the daemon at `socket` writes in answer to `request`, until
/// it closes the connection.
pub(super) async fn answer(socket: &Path, request: Request) -> Result<Vec<serde_json::Value>> {
    let mut stream = connect(socket).await?;
    protocol::write(&mut stream, &request).await?;
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

/// Write what the daemon at `socket` answers `request` with to `out`,
/// until either ends, running `started` once the answer starts, and pass
/// what comes `up` to the socket.
pub(super) async fn stream(
    socket: &Path,
    request: Request,
    out: &mut Outgoing,
    up: Option<Incoming>,
    started: Box<dyn FnOnce() + Send + '_>,
) -> Result<()> {
    let (reader, mut writer) = connect(socket).await?.into_split();
    protocol::write(&mut writer, &request).await?;
    // The answer decides when the stream ends; what the caller writes ends
    // it only by failing.
    tokio::select! {
        answered = pass_down(reader, out, started) => answered,
        Err(error) = pass_up(up, writer) => Err(error),
    }
}

async fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket)
        .await
        .context("could not reach this machine's daemon socket")
}

/// Write what the daemon answers on `socket` to `out`, until either ends,
/// running `started` once the answer starts.
async fn pass_down(
    mut socket: OwnedReadHalf,
    out: &mut Outgoing,
    started: Box<dyn FnOnce() + Send + '_>,
) -> Result<()> {
    let mut started = Some(started);
    let mut bytes = vec![0; READ_BYTES];
    loop {
        let read = tokio::select! {
            read = socket.read(&mut bytes) => read?,
            () = out.closed() => return Ok(()),
        };
        if read == 0 {
            return Ok(());
        }
        if let Some(started) = started.take() {
            started();
        }
        out.write(&bytes[..read]).await?;
    }
}

/// Write what the caller sends `up` a duplex stream to `socket`, closing
/// its side for writing once the caller ends; with nothing sent up, as on
/// a stream that is not duplex, never resolves. A socket that no longer
/// takes what comes up, as once the daemon's answer ended, takes nothing
/// more, and what the daemon still writes goes down.
async fn pass_up(up: Option<Incoming>, mut socket: OwnedWriteHalf) -> Result<()> {
    let Some(mut up) = up else {
        return std::future::pending().await;
    };
    while let Some(bytes) = up.next().await? {
        if socket.write_all(&bytes).await.is_err() {
            return std::future::pending().await;
        }
    }
    let _ = socket.shutdown().await;
    std::future::pending().await
}
