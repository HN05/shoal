//! A link's transport: its frames as lines of a byte stream, such as the
//! daemon's socket or a command's standard input and output, or as text
//! messages of a WebSocket. Each direction runs in a task of its own, so
//! that waiting for a frame never holds up sending one. The reader stops
//! with its channel; the writer once every sender is gone, after sending
//! what they queued.
use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    sync::mpsc,
    task::{AbortHandle, JoinHandle},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// The largest frame either side takes.
const MAX_FRAME: usize = 16 << 20;

/// Frames waiting to be sent before a sender waits.
const QUEUED: usize = 64;

/// How long closing a channel waits for its queued frames to go.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One side of an open transport.
pub(super) struct Channel {
    incoming: mpsc::Receiver<Result<String>>,
    outgoing: mpsc::Sender<String>,
    _reader: Reader,
    writer: JoinHandle<()>,
}

/// The task that reads a channel's frames, which ends with it.
struct Reader(AbortHandle);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What sends frames on a [`Channel`], which tasks other than its reader
/// hold.
#[derive(Clone)]
pub(super) struct Sender(mpsc::Sender<String>);

impl Sender {
    pub(super) async fn send(&self, frame: &impl Serialize) -> Result<()> {
        let text = serde_json::to_string(frame)?;
        (self.0.send(text).await).map_err(|_| anyhow::anyhow!("the link closed"))
    }
}

impl Channel {
    /// A channel over `reader` and `writer`, a frame per line.
    pub(super) fn lines(
        reader: impl AsyncBufRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        let (incoming, received) = mpsc::channel(QUEUED);
        let (outgoing, to_send) = mpsc::channel(QUEUED);
        Self {
            incoming: received,
            outgoing,
            _reader: Reader(tokio::spawn(read_lines(reader, incoming)).abort_handle()),
            writer: tokio::spawn(write_lines(writer, to_send)),
        }
    }

    /// A channel over `socket`, a frame per text message.
    pub(super) fn websocket<S>(socket: WebSocketStream<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        let (mut sink, mut stream) = socket.split();
        let (incoming, received) = mpsc::channel(QUEUED);
        let (outgoing, mut to_send) = mpsc::channel::<String>(QUEUED);
        let reader = tokio::spawn(async move {
            loop {
                let message = match stream.next().await {
                    None | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(Message::Text(text))) => Ok(text.as_str().to_owned()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Binary(_))) => Err(anyhow::anyhow!("a binary message")),
                    Some(Err(error)) => Err(error.into()),
                };
                let failed = message.is_err();
                if incoming.send(message).await.is_err() || failed {
                    return;
                }
            }
        });
        let writer = tokio::spawn(async move {
            while let Some(text) = to_send.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        Self {
            incoming: received,
            outgoing,
            _reader: Reader(reader.abort_handle()),
            writer,
        }
    }

    pub(super) fn sender(&self) -> Sender {
        Sender(self.outgoing.clone())
    }

    pub(super) async fn send(&self, frame: &impl Serialize) -> Result<()> {
        self.sender().send(frame).await
    }

    /// Send `frame`, then close the channel once it went, or the other side
    /// stopped reading.
    pub(super) async fn close_with(self, frame: &impl Serialize) {
        let _ = self.send(frame).await;
        let Self {
            outgoing, writer, ..
        } = self;
        drop(outgoing);
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, writer).await;
    }

    /// The next frame, none once the other side closed the link.
    pub(super) async fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        let Some(text) = self.incoming.recv().await else {
            return Ok(None);
        };
        let text = text?;
        let frame = serde_json::from_str(&text)
            .with_context(|| format!("unreadable hub link frame: {}", excerpt(&text)))?;
        Ok(Some(frame))
    }

    /// The next frame, which must come.
    pub(super) async fn expect<T: DeserializeOwned>(&mut self) -> Result<T> {
        match self.receive().await? {
            Some(frame) => Ok(frame),
            None => bail!("the link closed"),
        }
    }
}

/// The start of `text`, enough to tell what it is.
fn excerpt(text: &str) -> &str {
    let end = (text.char_indices().map(|(at, _)| at))
        .nth(200)
        .unwrap_or(text.len());
    &text[..end]
}

/// Pass each line of `reader` to `incoming`, until it ends or fails.
async fn read_lines(
    mut reader: impl AsyncBufRead + Send + Unpin,
    incoming: mpsc::Sender<Result<String>>,
) {
    loop {
        let line = read_line(&mut reader).await;
        let ended = !matches!(line, Ok(Some(_)));
        let Some(line) = line.transpose() else {
            return;
        };
        if incoming.send(line).await.is_err() || ended {
            return;
        }
    }
}

/// The next line of `reader`, none at its end.
async fn read_line(reader: &mut (impl AsyncBufRead + Send + Unpin)) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    (reader.take(MAX_FRAME as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.is_empty() {
        return Ok(None);
    }
    ensure!(
        bytes.len() <= MAX_FRAME,
        "a hub link frame exceeds {MAX_FRAME} bytes"
    );
    ensure!(
        bytes.last() == Some(&b'\n'),
        "the link closed partway through a frame"
    );
    bytes.pop();
    Ok(Some(String::from_utf8(bytes)?))
}

/// Write each frame `to_send` passes as a line of `writer`, until the
/// channel closes or a write fails.
async fn write_lines(
    mut writer: impl AsyncWrite + Send + Unpin,
    mut to_send: mpsc::Receiver<String>,
) {
    while let Some(mut text) = to_send.recv().await {
        text.push('\n');
        if writer.write_all(text.as_bytes()).await.is_err() || writer.flush().await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}
